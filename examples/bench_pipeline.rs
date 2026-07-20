// Full-pipeline throughput + correctness benchmark for the dispatcher's worker-metadata writes.
//
// Drives the *real* ventilator -> worker -> sink -> finalize loop over many tiny echo tasks, so the
// off-thread `WorkerMetadata::record_dispatched` / `record_received` writes (the thing Arm 14 #4
// changed from a fresh per-event `PgConnection` to a pooled checkout) are exercised at pipeline
// rate.
//
// It reports, for the run:
//   - wall-clock + tasks/second through the live pipeline
//   - worker_metadata totals (total_dispatched / total_returned) vs the task count (the OLD path
//     calls `PgConnection::establish(...).expect(...)`, so under connection-storm it PANICS in the
//     detached thread and silently DROPS the metadata write -> totals come up short; the pooled
//     path caps connections and records every event -> exact totals)
//
// A/B: the pooled arm is current HEAD. For the unpooled (pre-#4) baseline, revert ONLY these four
// files, rebuild the example, run; then restore them and rebuild:
//   - src/models/worker_metadata.rs
//   - src/dispatcher/ventilator.rs
//   - src/dispatcher/sink.rs
//   - src/dispatcher/manager.rs
// (`git checkout HEAD~1 -- <those 4>` for the baseline, `git checkout HEAD -- <those 4>` to
// restore.)
//
// The run is time-boxed: it loads a big backlog of TODO tasks, lets the live pipeline churn for
// BENCH_SECONDS, then counts what flowed through. A fixed window (rather than a job_limit) avoids
// the prototype's mock-reply / lockstep termination fragility and measures steady-state throughput.
//
// Env knobs: BENCH_TASKS backlog (default 20000), BENCH_WORKERS (default 1),
//            BENCH_SECONDS window (default 15), BENCH_LABEL (default "run").

use std::env;
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use cortex::backend;
use cortex::backend::test_db_address;
use cortex::dispatcher::manager::TaskManager;
use cortex::helpers::TaskStatus;
use cortex::models::{Corpus, NewCorpus, NewService, NewTask, Service};
use pericortex::worker::{EchoWorker, Worker};

use cortex::schema::{corpora, services, tasks, worker_metadata};
use diesel::dsl::sql;
use diesel::prelude::*;
use diesel::sql_types::BigInt;

const SOURCE_PORT: usize = 53695;
const RESULT_PORT: usize = 53696;
const CORPUS_NAME: &str = "bench-pipeline corpus";
const SERVICE_NAME: &str = "bench_echo";
const SCRATCH: &str = "/tmp/cortex_bench";

fn env_usize(key: &str, default: usize) -> usize {
  env::var(key)
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(default)
}

fn main() {
  let n_tasks = env_usize("BENCH_TASKS", 20000);
  let n_workers = env_usize("BENCH_WORKERS", 1);
  let window = Duration::from_secs(env_usize("BENCH_SECONDS", 15) as u64);
  let label = env::var("BENCH_LABEL").unwrap_or_else(|_| "run".to_string());

  let mut backend = backend::testdb();

  // --- Clean slate -----------------------------------------------------------------------------
  // Throwaway test DB: wipe worker_metadata so the totals reflect only this run.
  diesel::delete(worker_metadata::table)
    .execute(&mut backend.connection)
    .expect("reset worker_metadata");
  diesel::delete(corpora::table)
    .filter(corpora::name.eq(CORPUS_NAME))
    .execute(&mut backend.connection)
    .ok();
  diesel::delete(services::table)
    .filter(services::name.eq(SERVICE_NAME))
    .execute(&mut backend.connection)
    .ok();

  // --- Stage N tiny payloads, one per (distinct) entry -----------------------------------------
  // Tiny + invalid-as-zip is fine: generate_report tolerates it (default-Fatal, no panic), and the
  // metadata writes -- the thing under test -- fire regardless of report content. Tiny keeps the
  // round-trip fast so the metadata-write rate (and any connection storm) is high.
  fs::remove_dir_all(SCRATCH).ok();
  fs::create_dir_all(SCRATCH).expect("scratch root");
  let mut new_tasks: Vec<NewTask> = Vec::with_capacity(n_tasks);
  backend
    .add(&NewCorpus {
      name: CORPUS_NAME.to_string(),
      path: SCRATCH.to_string(),
      complex: true,
      description: String::new(),
    })
    .expect("add corpus");
  let corpus = Corpus::find_by_name(CORPUS_NAME, &mut backend.connection).expect("find corpus");

  backend
    .add(&NewService {
      name: SERVICE_NAME.to_string(),
      version: 0.1,
      inputformat: "tex".to_string(),
      outputformat: "tex".to_string(),
      inputconverter: Some("import".to_string()),
      complex: true,
      description: String::from("pipeline benchmark echo service"),
    })
    .expect("add service");
  let service = Service::find_by_name(SERVICE_NAME, &mut backend.connection).expect("find service");

  // Flat files in one dir (distinct entries satisfy the UNIQUE(entry,service,corpus) constraint)
  // keep the filesystem-op count low; the single sink writes the shared result file serially.
  for i in 0..n_tasks {
    let entry: PathBuf = [SCRATCH, &format!("{i}.zip")].iter().collect();
    fs::write(&entry, b"PK\x03\x04bench").expect("write payload");
    new_tasks.push(NewTask {
      entry: entry.to_str().unwrap().to_string(),
      service_id: service.id,
      corpus_id: corpus.id,
      status: TaskStatus::TODO.raw(),
    });
  }
  // Chunk inserts: NewTask binds 4 params/row, and Postgres caps a statement at 65535 bind params.
  for chunk in new_tasks.chunks(10_000) {
    diesel::insert_into(tasks::table)
      .values(chunk)
      .execute(&mut backend.connection)
      .expect("bulk insert tasks");
  }

  let window_s = window.as_secs();
  println!(
    "[{label}] staged {n_tasks} TODO tasks, {n_workers} worker(s); running pipeline for {window_s}s..."
  );

  // --- Drive the live pipeline (detached, time-boxed) ------------------------------------------
  // job_limit = None: ventilator/sink/finalize loop forever; we stop measuring after the window and
  // process::exit, abandoning the still-running threads. No join, so no lockstep termination hang.
  thread::spawn(move || {
    TaskManager {
      source_port: SOURCE_PORT,
      result_port: RESULT_PORT,
      queue_size: 100,
      message_size: 100_000,
      backend_address: test_db_address().to_string(),
      ..TaskManager::default()
    }
    .start(None)
    .expect("manager start");
  });

  // D-21: `start()` would overwrite `identity` with `<host>:<service>:<pid>`, identical for every
  // thread of this process, collapsing the "fleet" onto ONE ZMQ identity under the ventilator's
  // `router_handover` — so this bench would measure a single peer no matter what `n_workers` says,
  // and would attribute the resulting dropped dispatches to the dispatcher. `start_single()` is
  // what `start()` calls after setting the identity, so it keeps the distinct one built here.
  let fleet_pid = std::process::id();
  for w in 0..n_workers {
    thread::spawn(move || {
      let worker = EchoWorker {
        service: SERVICE_NAME.to_string(),
        version: 0.1,
        message_size: 100_000,
        source: format!("tcp://127.0.0.1:{SOURCE_PORT}"),
        sink: format!("tcp://127.0.0.1:{RESULT_PORT}"),
        identity: format!("bench-fleet:{SERVICE_NAME}:{fleet_pid}-{w:02}"),
      };
      worker.start_single(None).ok();
    });
  }

  let start = Instant::now();
  thread::sleep(window);
  let elapsed = start.elapsed();

  // --- Results ---------------------------------------------------------------------------------
  // Pipeline throughput: tasks finalize moved off TODO (status persisted) within the window.
  let completed: i64 = tasks::table
    .filter(tasks::corpus_id.eq(corpus.id))
    .filter(tasks::service_id.eq(service.id))
    .filter(tasks::status.ne(TaskStatus::TODO.raw()))
    .count()
    .get_result(&mut backend.connection)
    .unwrap_or(0);

  // Metadata-subsystem completeness: events the (un)pooled writers actually recorded.
  let (dispatched, returned): (Option<i64>, Option<i64>) = worker_metadata::table
    .filter(worker_metadata::service_id.eq(service.id))
    .select((
      sql::<diesel::sql_types::Nullable<BigInt>>("SUM(total_dispatched)"),
      sql::<diesel::sql_types::Nullable<BigInt>>("SUM(total_returned)"),
    ))
    .first(&mut backend.connection)
    .unwrap_or((None, None));
  let dispatched = dispatched.unwrap_or(0);
  let returned = returned.unwrap_or(0);

  let secs = elapsed.as_secs_f64();
  let rate = completed as f64 / secs;
  let backlog_note = if completed as usize >= n_tasks {
    "  (WARNING: backlog drained -- raise BENCH_TASKS; throughput is backlog-limited)"
  } else {
    ""
  };
  println!("\n========== bench_pipeline [{label}] ==========");
  println!("window                         : {secs:.1} s   workers: {n_workers}");
  println!("tasks completed (status moved) : {completed}{backlog_note}");
  println!("pipeline throughput            : {rate:.1} tasks/s");
  println!("metadata recorded (best-effort, racy -- informational only):");
  println!("    dispatched {dispatched} / returned {returned}");
  // Secondary drop signal lives in stderr: the unpooled path's
  // `PgConnection::establish(...).expect(...)` panics ("Error connecting to ...") in its detached
  // thread when connections exhaust. Count those to compare arms.
  println!("(also: grep stderr for \"Error connecting\" panics = unpooled connection exhaustion)");
  println!("==============================================");

  std::process::exit(0);
}
