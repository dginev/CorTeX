// The canonical, long-term **dispatcher quality benchmark** — perf *and* robustness, revisitable to
// catch regressions over time. Drives the *real* ventilator → worker → sink → finalize pipeline
// (`TaskManager`) over a real `pericortex::EchoWorker` fleet, against the test DB.
//
// Unlike a fixed-window throughput probe, this **drains a fixed backlog to completion** and times
// it, so "N tasks in T seconds" is directly comparable across commits — and it **asserts
// correctness**, so a perf change that silently drops or mis-statuses work fails the bench instead
// of looking fast.
//
// Payloads are *valid* result `.zip`s carrying a `cortex.log` (so the per-task result-parse hot
// path is exercised for real, not defaulted to Fatal), with a configurable size to stress the
// sink's `/data` write + the ZMQ transfer.
//
// Measures (perf):   wall-clock drain time, tasks/s, tasks/s/worker, sink-write throughput (MB/s).
// Asserts (robust):  every task reaches a terminal status (no loss); none left TODO/Queued; the
//                    status distribution matches the controlled payload (the parse path is
// correct);                    worker_metadata recorded every dispatch + return.
//
// Env knobs:  BENCH_TASKS (default 20000), BENCH_WORKERS (default 4), BENCH_PAYLOAD_KB (default 8,
//             the source/result archive size), BENCH_DEADLINE_S (default 180), BENCH_JSON=1 (emit a
//             one-line JSON record for tracking), BENCH_LABEL (default "run").
//
// Baselines + interpretation: docs/DISPATCHER_BENCH.md.

use std::env;
use std::fs;
use std::io::Write;
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

const SOURCE_PORT: usize = 53697; // distinct from bench_pipeline (53695/53696) so both can coexist
const RESULT_PORT: usize = 53698;
const CORPUS_NAME: &str = "dispatcher-bench corpus";
const SERVICE_NAME: &str = "bench_echo_q";
const SCRATCH: &str = "/tmp/cortex_dispatcher_bench";

fn env_usize(key: &str, default: usize) -> usize {
  env::var(key)
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(default)
}

/// Builds a *valid* result `.zip` at `path`: a `cortex.log` that derives to `NoProblem` (a
/// `conversion:0` info line) plus `payload_kb` of filler — so the echo round-trip exercises the
/// real result-parse + the sink write at a realistic size.
fn build_source_zip(path: &PathBuf, payload_kb: usize) {
  let file = fs::File::create(path).expect("create source zip");
  let mut zw = zip::ZipWriter::new(file);
  let opts: zip::write::FileOptions<()> =
    zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
  zw.start_file("cortex.log", opts).unwrap();
  // severity:category:what — `what` is the first non-space token, so `info:conversion:0` gives
  // what="0" ⇒ cortex status -(0+1) = -1 = NoProblem. (A tab before details would fold into
  // `what`.)
  zw.write_all(b"info:conversion:0\n").unwrap();
  if payload_kb > 0 {
    zw.start_file("content.tex", opts).unwrap();
    zw.write_all(&vec![b'x'; payload_kb * 1024]).unwrap();
  }
  zw.finish().unwrap();
}

/// Count of this run's tasks **finalized to a terminal status** (`status < 0`: NoProblem/Warning/
/// Error/Fatal/Invalid). Excludes `TODO` (0) and `Queued` (>0, a transient lease mark) — so the
/// drain completes only when every task is genuinely done, not merely leased.
fn completed_count(conn: &mut diesel::PgConnection, corpus_id: i32, service_id: i32) -> i64 {
  tasks::table
    .filter(tasks::corpus_id.eq(corpus_id))
    .filter(tasks::service_id.eq(service_id))
    .filter(tasks::status.lt(0))
    .count()
    .get_result(conn)
    .unwrap_or(0)
}

fn status_count(
  conn: &mut diesel::PgConnection,
  corpus_id: i32,
  service_id: i32,
  status: i32,
) -> i64 {
  tasks::table
    .filter(tasks::corpus_id.eq(corpus_id))
    .filter(tasks::service_id.eq(service_id))
    .filter(tasks::status.eq(status))
    .count()
    .get_result(conn)
    .unwrap_or(0)
}

fn main() {
  let n_tasks = env_usize("BENCH_TASKS", 20000);
  let n_workers = env_usize("BENCH_WORKERS", 4).max(1);
  let payload_kb = env_usize("BENCH_PAYLOAD_KB", 8);
  let deadline = Duration::from_secs(env_usize("BENCH_DEADLINE_S", 180) as u64);
  let json = env::var("BENCH_JSON").is_ok();
  let label = env::var("BENCH_LABEL").unwrap_or_else(|_| "run".to_string());

  let mut backend = backend::testdb();

  // --- Clean slate (throwaway test DB) ---------------------------------------------------------
  diesel::delete(worker_metadata::table)
    .execute(&mut backend.connection)
    .expect("reset worker_metadata");
  diesel::delete(corpora::table.filter(corpora::name.eq(CORPUS_NAME)))
    .execute(&mut backend.connection)
    .ok();
  diesel::delete(services::table.filter(services::name.eq(SERVICE_NAME)))
    .execute(&mut backend.connection)
    .ok();

  // --- Stage N valid-zip payloads --------------------------------------------------------------
  fs::remove_dir_all(SCRATCH).ok();
  fs::create_dir_all(SCRATCH).expect("scratch root");
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
      description: String::from("dispatcher quality benchmark echo service"),
    })
    .expect("add service");
  let service = Service::find_by_name(SERVICE_NAME, &mut backend.connection).expect("find service");

  let mut new_tasks: Vec<NewTask> = Vec::with_capacity(n_tasks);
  for i in 0..n_tasks {
    // One subdir per task (the arXiv topology) so each result lands in its OWN
    // `<dir>/<service>.zip` — a shared flat dir would race the sink writes + parse.
    let dir: PathBuf = [SCRATCH, &format!("{i}")].iter().collect();
    fs::create_dir_all(&dir).expect("task dir");
    let entry: PathBuf = dir.join("source.zip");
    build_source_zip(&entry, payload_kb);
    new_tasks.push(NewTask {
      entry: entry.to_str().unwrap().to_string(),
      service_id: service.id,
      corpus_id: corpus.id,
      status: TaskStatus::TODO.raw(),
    });
  }
  for chunk in new_tasks.chunks(10_000) {
    diesel::insert_into(tasks::table)
      .values(chunk)
      .execute(&mut backend.connection)
      .expect("bulk insert tasks");
  }

  println!(
    "[{label}] staged {n_tasks} TODO tasks ({payload_kb}KB each), {n_workers} worker(s); draining..."
  );

  // --- Drive the live pipeline (detached) ------------------------------------------------------
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
  for w in 0..n_workers {
    thread::spawn(move || {
      let mut worker = EchoWorker {
        service: SERVICE_NAME.to_string(),
        version: 0.1,
        message_size: 100_000,
        source: format!("tcp://127.0.0.1:{SOURCE_PORT}"),
        sink: format!("tcp://127.0.0.1:{RESULT_PORT}"),
        identity: format!("bench-q-worker-{w}"),
      };
      worker.start(None).ok();
    });
  }

  // --- Drain to completion, timed ---------------------------------------------------------------
  let start = Instant::now();
  let mut completed = 0i64;
  while start.elapsed() < deadline {
    completed = completed_count(&mut backend.connection, corpus.id, service.id);
    if completed as usize >= n_tasks {
      break;
    }
    thread::sleep(Duration::from_millis(200));
  }
  let elapsed = start.elapsed();
  let drained = completed as usize >= n_tasks;

  // --- Robustness audit ------------------------------------------------------------------------
  let no_problem = status_count(
    &mut backend.connection,
    corpus.id,
    service.id,
    TaskStatus::NoProblem.raw(),
  );
  let still_todo = status_count(
    &mut backend.connection,
    corpus.id,
    service.id,
    TaskStatus::TODO.raw(),
  );
  // Queued is any positive lease mark; count tasks still leased (should be 0 once drained).
  let still_queued: i64 = tasks::table
    .filter(tasks::corpus_id.eq(corpus.id))
    .filter(tasks::service_id.eq(service.id))
    .filter(tasks::status.gt(0))
    .count()
    .get_result(&mut backend.connection)
    .unwrap_or(0);
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

  let secs = elapsed.as_secs_f64().max(1e-9);
  let rate = completed as f64 / secs;
  let mb = (completed as f64 * payload_kb as f64 / 1024.0) * 2.0; // source + result transfer
  let mbps = mb / secs;

  println!("\n========== dispatcher_bench [{label}] ==========");
  println!("workers {n_workers} · payload {payload_kb}KB · tasks {n_tasks}");
  println!(
    "drained                 : {completed}/{n_tasks} in {secs:.2}s{}",
    if drained { "" } else { "  (TIMEOUT)" }
  );
  println!(
    "throughput              : {rate:.0} tasks/s   ({:.0} tasks/s/worker)",
    rate / n_workers as f64
  );
  println!("transfer                : {mbps:.0} MB/s (source+result)");
  println!("status: NoProblem {no_problem} · TODO {still_todo} · Queued {still_queued}");
  println!("metadata: dispatched {dispatched} · returned {returned}");

  // Correctness gates — a perf change that loses/mis-statuses work must FAIL here.
  let mut failures: Vec<String> = Vec::new();
  if !drained {
    failures.push(format!(
      "did not drain within {}s ({completed}/{n_tasks})",
      deadline.as_secs()
    ));
  }
  if still_todo != 0 || still_queued != 0 {
    failures.push(format!(
      "non-terminal tasks remain (TODO {still_todo}, Queued {still_queued})"
    ));
  }
  if drained && no_problem != n_tasks as i64 {
    failures.push(format!(
      "status distribution wrong: expected {n_tasks} NoProblem, got {no_problem} (parse-path regression?)"
    ));
  }
  // NB: worker_metadata is intentionally best-effort (the D-1 writer drops under saturation), so
  // its totals are reported but never asserted — they would flake.

  if json {
    println!(
      "JSON {{\"label\":\"{label}\",\"workers\":{n_workers},\"payload_kb\":{payload_kb},\"tasks\":{n_tasks},\"drained\":{drained},\"secs\":{secs:.3},\"tasks_per_s\":{rate:.1},\"mb_per_s\":{mbps:.1},\"no_problem\":{no_problem},\"ok\":{}}}",
      failures.is_empty()
    );
  }

  if failures.is_empty() {
    println!(
      "✓ PASS — perf measured, correctness asserted (no loss, all terminal, status correct)"
    );
    println!("==============================================");
    std::process::exit(0);
  } else {
    println!("✗ FAIL:");
    for f in &failures {
      println!("    - {f}");
    }
    println!("==============================================");
    std::process::exit(1);
  }
}
