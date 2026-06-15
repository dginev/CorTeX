// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Concurrent end-to-end gate for the rationalized dispatcher (phases 1–4: bounded done-channel,
//! batched finalize, sink writer fan-out, lock-free in-flight/service maps).
//!
//! The deployment sizing is ~200 concurrent workers at ~100 tasks/s, so the full
//! ventilator → sink (writer pool) → finalize pipeline must drive **200 real tasks** to completion
//! under **many concurrent workers** with **zero loss** and **byte-exact** results — exactly the
//! contention the phase-4 `DashMap` in-flight set + `AtomicUsize` counter and the phase-3 writer
//! pool exist to handle. It complements the single-task `echo_roundtrip` and the malformed-flood
//! `dispatcher_torture_test`.
//!
//! Runs the **perpetual** dispatcher (`job_limit = None`) + N `EchoWorker`s and polls the task
//! store until every task reaches a terminal status, rather than using a finite `job_limit` (whose
//! three-thread lockstep can hang — KNOWN_ISSUES D-5), mirroring `dispatcher_bench` /
//! `dispatcher_torture_test`.
//!
//! Custom harness (KNOWN_ISSUES L-1): own `main` + `_exit(0)`. Knobs: `CONCURRENCY_TASKS` (default
//! 200), `CONCURRENCY_WORKERS` (default 8), `CONCURRENCY_DEADLINE_SECS` (default 90),
//! `CORTEX_DISPATCHER__SINK_WRITERS`, `CORTEX_WORKER_THROTTLE_SECS`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
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

const SOURCE_PORT: usize = 55695;
const RESULT_PORT: usize = 55696;
const CORPUS_NAME: &str = "concurrent dispatch corpus";
const SERVICE_NAME: &str = "concurrent_echo";
const SCRATCH: &str = "/tmp/cortex_concurrent";

/// Writes a `.zip` at `path` carrying a `cortex.log` (→ NoProblem) + `filler` filler bytes.
fn build_zip(path: &Path, filler: usize) {
  let file = fs::File::create(path).expect("create zip");
  let mut zw = zip::ZipWriter::new(file);
  let opts: zip::write::FileOptions<()> =
    zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
  zw.start_file("cortex.log", opts).unwrap();
  zw.write_all(b"info:conversion:0\n").unwrap();
  if filler > 0 {
    zw.start_file("content.bin", opts).unwrap();
    zw.write_all(&vec![b'x'; filler]).unwrap();
  }
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
  build_zip(&entry, 4096);
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

fn env_usize(key: &str, default: usize) -> usize {
  std::env::var(key)
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(default)
}

fn main() {
  let n_tasks = env_usize("CONCURRENCY_TASKS", 200);
  let n_workers = env_usize("CONCURRENCY_WORKERS", 8);
  let deadline = Duration::from_secs(env_usize("CONCURRENCY_DEADLINE_SECS", 90) as u64);
  // A worker that momentarily finds the queue empty naps `CORTEX_WORKER_THROTTLE_SECS` (default
  // 60); keep it short so the tail of the drain isn't dominated by a nap. Must be set before the
  // worker threads read it.
  if std::env::var("CORTEX_WORKER_THROTTLE_SECS").is_err() {
    std::env::set_var("CORTEX_WORKER_THROTTLE_SECS", "1");
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
      description: "concurrent echo".into(),
    })
    .expect("add service");
  let service = Service::find_by_name(SERVICE_NAME, &mut backend.connection).unwrap();

  let entries: Vec<String> = (0..n_tasks)
    .map(|i| stage_task(&mut backend, &corpus, &service, &format!("task{i}")))
    .collect();
  println!("[concurrent] staged {n_tasks} tasks; starting dispatcher + {n_workers} echo workers");

  // Perpetual dispatcher (job_limit = None) — avoids the finite-job_limit lockstep hang (D-5).
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

  // A fleet of concurrent echo workers, each with a distinct ROUTER identity.
  for w in 0..n_workers {
    thread::spawn(move || {
      EchoWorker {
        service: SERVICE_NAME.to_string(),
        version: 0.1,
        message_size: 100_000,
        source: format!("tcp://127.0.0.1:{SOURCE_PORT}"),
        sink: format!("tcp://127.0.0.1:{RESULT_PORT}"),
        identity: format!("concurrent-echo-worker-{w}"),
      }
      .start(None)
      .ok();
    });
  }

  // Drain: every task must reach a terminal status within the deadline.
  let start = Instant::now();
  let mut drained = false;
  while start.elapsed() < deadline {
    let terminal = entries
      .iter()
      .filter(|e| status_of(&mut backend.connection, e, service.id) < 0)
      .count();
    if terminal == n_tasks {
      drained = true;
      println!(
        "[concurrent] all {n_tasks} tasks terminal in {:?}",
        start.elapsed()
      );
      break;
    }
    thread::sleep(Duration::from_millis(200));
  }

  if !drained {
    let mut todo = 0;
    let mut queued = 0;
    let mut terminal = 0;
    for e in &entries {
      match status_of(&mut backend.connection, e, service.id) {
        0 => todo += 1,
        s if s > 0 => queued += 1,
        _ => terminal += 1,
      }
    }
    eprintln!("[diag] at deadline: terminal={terminal} todo={todo} queued={queued} of {n_tasks}");
  }
  assert!(
    drained,
    "CONCURRENCY: not all {n_tasks} tasks finalized under {n_workers} concurrent workers (loss/stall?)"
  );

  // No loss + correctness: every task NoProblem, and every result a byte-exact echo of its source
  // (the lock-free in-flight set must never splice or drop a result under concurrency).
  for entry in &entries {
    let status = status_of(&mut backend.connection, entry, service.id);
    assert_eq!(
      status,
      TaskStatus::NoProblem.raw(),
      "CONCURRENCY: task finalized to {status}, not NoProblem ({entry})"
    );
    let source_bytes = fs::read(entry).expect("read source zip");
    let result_bytes = fs::read(result_path(entry)).expect("read result zip");
    assert_eq!(
      result_bytes, source_bytes,
      "CONCURRENCY: result for {entry} is not a byte-exact echo of its source"
    );
  }
  println!(
    "✓ concurrency: all {n_tasks} tasks NoProblem + byte-exact echoes under {n_workers} workers (zero loss)"
  );

  fs::remove_dir_all(SCRATCH).ok();
  eprintln!("concurrent_dispatch_test: all cases passed");
  unsafe { libc::_exit(0) }
}
