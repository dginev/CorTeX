// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! KNOWN_ISSUES **D-5** regression: a **bounded** dispatcher run (`job_limit = Some(N)`) must
//! terminate **cleanly** — the case that used to *deadlock on shutdown*.
//!
//! The original hang came from the three pipeline threads counting `job_limit` in three
//! incompatible units: the ventilator counted **requests** (so mock-replies inflated its tally to
//! `N` while it had dispatched *fewer* than `N` real tasks), the sink counted **results received**,
//! and finalize counted **coalesced batches**. With `N` larger than the available work the
//! ventilator would stop after `N` requests while the sink blocked forever waiting for `N` results
//! that were never dispatched, and finalize blocked waiting for `N` batches that a handful of large
//! batches never reached — `manager.start(Some(N))` never returned.
//!
//! This exercises exactly that shape — **`SEEDED` (< `JOB_LIMIT`) real tasks**, a *perpetual* echo
//! worker that keeps requesting (so the ventilator sees the source drain), and a bounded manager —
//! and asserts the manager **returns within a deadline** (a hang is a hard failure, not an infinite
//! wait) and that every seeded task finalized to a byte-exact `NoProblem` echo. The fix: one shared
//! `dispatch_complete` signal + an in-flight-set drain + a `Disconnected`-driven finalize shutdown,
//! replacing the three mismatched counters.
//!
//! Custom harness (KNOWN_ISSUES L-1): own `main` + `_exit(0)` so the still-live detached worker /
//! tokio / r2d2 threads can't race the C `atexit` cleanup into a teardown SIGSEGV.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use cortex::backend;
use cortex::backend::test_db_address;
use cortex::dispatcher::manager::TaskManager;
use cortex::helpers::TaskStatus;
use cortex::models::{Corpus, NewCorpus, NewService, NewTask, Service};
use cortex::schema::{corpora, services, tasks};
use diesel::prelude::*;
use pericortex::worker::{EchoWorker, Worker};

const SOURCE_PORT: usize = 56695;
const RESULT_PORT: usize = 56696;
const CORPUS_NAME: &str = "d5 job-limit corpus";
const SERVICE_NAME: &str = "d5_job_limit_echo";
const SCRATCH: &str = "/tmp/cortex_d5_job_limit";
/// Real TODO tasks staged for the run.
const SEEDED: usize = 8;
/// Bounded limit, deliberately **larger** than `SEEDED` so the run must drain the source (and the
/// old request/batch counters would inflate past the real work) — the precise deadlock condition.
const JOB_LIMIT: usize = 50;

/// Writes a `.zip` at `path` carrying a `cortex.log` (→ `NoProblem`) so the echoed result finalizes
/// to a real terminal status.
fn build_zip(path: &Path) {
  let file = fs::File::create(path).expect("create zip");
  let mut zw = zip::ZipWriter::new(file);
  let opts: zip::write::FileOptions<()> =
    zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
  zw.start_file("cortex.log", opts).unwrap();
  zw.write_all(b"info:conversion:0\n").unwrap();
  zw.finish().unwrap();
}

fn stage_task(
  backend: &mut backend::Backend,
  corpus: &Corpus,
  service: &Service,
  name: &str,
) -> String {
  let dir: PathBuf = [SCRATCH, name].iter().collect();
  fs::create_dir_all(&dir).unwrap();
  let entry = dir.join("source.zip");
  build_zip(&entry);
  let entry_str = entry.to_str().unwrap().to_string();
  backend
    .add(&NewTask {
      entry: entry_str.clone(),
      service_id: service.id,
      corpus_id: corpus.id,
      status: TaskStatus::TODO.raw(),
    })
    .expect("stage task");
  entry_str
}

fn status_of(conn: &mut diesel::PgConnection, entry: &str, service_id: i32) -> i32 {
  tasks::table
    .filter(tasks::entry.eq(entry))
    .filter(tasks::service_id.eq(service_id))
    .select(tasks::status)
    .first::<i32>(conn)
    .unwrap_or(0)
}

fn result_path(entry: &str) -> PathBuf {
  Path::new(entry)
    .parent()
    .unwrap()
    .join(format!("{SERVICE_NAME}.zip"))
}

fn main() {
  // A worker that finds the queue empty naps `CORTEX_WORKER_THROTTLE_SECS` before re-requesting;
  // keep it short so the drain-detection (which needs the worker to keep requesting after the
  // source empties) isn't dominated by a 60 s nap. Must be set before the worker thread reads it.
  if std::env::var("CORTEX_WORKER_THROTTLE_SECS").is_err() {
    // FIXME: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var("CORTEX_WORKER_THROTTLE_SECS", "1") };
  }

  let mut backend = backend::testdb();
  // Clean slate.
  diesel::delete(corpora::table.filter(corpora::name.eq(CORPUS_NAME)))
    .execute(&mut backend.connection)
    .ok();
  diesel::delete(services::table.filter(services::name.eq(SERVICE_NAME)))
    .execute(&mut backend.connection)
    .ok();
  fs::remove_dir_all(SCRATCH).ok();
  fs::create_dir_all(SCRATCH).unwrap();

  backend
    .add(&NewCorpus {
      name: CORPUS_NAME.into(),
      path: SCRATCH.into(),
      complex: true,
      description: String::new(),
    })
    .expect("add corpus");
  let corpus = Corpus::find_by_name(CORPUS_NAME, &mut backend.connection).unwrap();
  backend
    .add(&NewService {
      name: SERVICE_NAME.into(),
      version: 0.1,
      inputformat: "tex".into(),
      outputformat: "tex".into(),
      inputconverter: Some("import".into()),
      complex: true,
      description: "d5 job-limit echo".into(),
    })
    .expect("add service");
  let service = Service::find_by_name(SERVICE_NAME, &mut backend.connection).unwrap();

  let entries: Vec<String> = (0..SEEDED)
    .map(|i| stage_task(&mut backend, &corpus, &service, &format!("task{i}")))
    .collect();
  println!(
    "[d5] staged {SEEDED} TODO tasks; bounded manager job_limit={JOB_LIMIT} (> seeded), 1 perpetual echo worker"
  );

  // A perpetual echo worker (it must keep requesting *after* the {SEEDED} real tasks so the
  // ventilator observes the source drain). Detached — the bounded manager, not the worker, is what
  // we assert terminates.
  thread::spawn(move || {
    EchoWorker {
      service: SERVICE_NAME.to_string(),
      version: 0.1,
      message_size: 100_000,
      source: format!("tcp://127.0.0.1:{SOURCE_PORT}"),
      sink: format!("tcp://127.0.0.1:{RESULT_PORT}"),
      identity: "d5-echo-worker".to_string(),
    }
    .start(None)
    .ok();
  });

  // The bounded manager under test — run in a joinable thread, with a shared "returned" flag so the
  // main thread can enforce a deadline (a hang must FAIL, not block CI forever).
  let manager_returned = Arc::new(AtomicBool::new(false));
  let manager_flag = manager_returned.clone();
  let manager_thread = thread::spawn(move || {
    TaskManager {
      source_port: SOURCE_PORT,
      result_port: RESULT_PORT,
      queue_size: 100,
      message_size: 100_000,
      backend_address: test_db_address().to_string(),
      ..TaskManager::default()
    }
    .start(Some(JOB_LIMIT))
    .expect("D-5: bounded manager.start(Some(N)) must return Ok, not hang");
    manager_flag.store(true, Ordering::SeqCst);
  });

  // The crux: the bounded run must terminate. Generous deadline (the happy path is ~1-2 s with the
  // 1 s worker throttle); exceeding it means the lockstep hang has regressed.
  let deadline = Duration::from_secs(60);
  let start = Instant::now();
  while start.elapsed() < deadline && !manager_returned.load(Ordering::SeqCst) {
    thread::sleep(Duration::from_millis(100));
  }
  assert!(
    manager_returned.load(Ordering::SeqCst),
    "D-5 REGRESSION: bounded job_limit={JOB_LIMIT} run did not terminate within {deadline:?} — the lockstep shutdown hang is back"
  );
  manager_thread.join().expect("manager thread panicked");
  println!(
    "✓ d5: bounded manager terminated cleanly in {:?}",
    start.elapsed()
  );

  // Correctness: the manager joins finalize before returning, so by now every seeded task is
  // persisted — each a byte-exact `NoProblem` echo (no work lost or stranded by the new shutdown).
  for entry in &entries {
    let status = status_of(&mut backend.connection, entry, service.id);
    assert_eq!(
      status,
      TaskStatus::NoProblem.raw(),
      "D-5: seeded task did not finalize to NoProblem (status {status}): {entry}"
    );
    let source_bytes = fs::read(entry).expect("read source zip");
    let result_bytes = fs::read(result_path(entry)).expect("read result zip");
    assert_eq!(
      result_bytes, source_bytes,
      "D-5: result for {entry} is not a byte-exact echo of its source"
    );
  }
  println!(
    "✓ d5: all {SEEDED} seeded tasks finalized NoProblem with byte-exact echoes (none stranded)"
  );

  fs::remove_dir_all(SCRATCH).ok();
  eprintln!("dispatcher_job_limit_test: all cases passed");
  unsafe { libc::_exit(0) }
}
