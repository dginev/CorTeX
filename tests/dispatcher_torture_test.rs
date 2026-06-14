// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Dispatcher robustness torture tests against the *real* `TaskManager` + `EchoWorker`:
//!   1. A **barrage of bad/empty/malformed replies** to the sink (no frames, id-only, truncated
//!      envelope, bogus task ids) injected concurrently with real work — asserts the sink survives
//!      and does not desync (every real task still finalizes). This is the regression guard for the
//!      `RCVMORE`-checked envelope hardening.
//!   2. The **hard result-size cap** (`dispatcher.max_result_bytes`): a result under the cap is
//!      accepted + written; a result over the cap is rejected (task `Invalid`, no oversized file
//!      left on disk). Fast by default (1 MiB cap, KB–MB payloads); set `CORTEX_TORTURE_BIG=1` to
//!      exercise the real **2 GiB-accepted / 10 GiB-rejected** sizes (heavy — multi-GB I/O; payload
//!      cleaned up).
//!
//! Custom harness (KNOWN_ISSUES L-1): own `main` + `_exit(0)`.

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

const SOURCE_PORT: usize = 54695;
const RESULT_PORT: usize = 54696;
const CORPUS_NAME: &str = "torture corpus";
const SERVICE_NAME: &str = "torture_echo";
const SCRATCH: &str = "/tmp/cortex_torture";

/// Writes a `.zip` at `path` carrying a `cortex.log` (→ NoProblem) + `filler` bytes of content,
/// streamed in chunks so a multi-GB payload never sits resident.
fn build_zip(path: &Path, filler: usize) {
  let file = fs::File::create(path).expect("create zip");
  let mut zw = zip::ZipWriter::new(file);
  let opts: zip::write::FileOptions<()> =
    zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
  zw.start_file("cortex.log", opts).unwrap();
  zw.write_all(b"info:conversion:0\n").unwrap();
  if filler > 0 {
    zw.start_file("content.bin", opts).unwrap();
    let chunk = vec![b'x'; 1024 * 1024];
    let mut written = 0;
    while written < filler {
      let n = (filler - written).min(chunk.len());
      zw.write_all(&chunk[..n]).unwrap();
      written += n;
    }
  }
  zw.finish().unwrap();
}

fn stage_task(
  backend: &mut backend::Backend,
  corpus: &Corpus,
  service: &Service,
  name: &str,
  filler: usize,
) -> String {
  let dir: PathBuf = [SCRATCH, name].iter().collect();
  fs::create_dir_all(&dir).unwrap();
  let entry = dir.join("source.zip");
  build_zip(&entry, filler);
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

/// Result file the sink writes for a task whose source is `<dir>/source.zip`.
fn result_path(entry: &str) -> PathBuf {
  Path::new(entry)
    .parent()
    .unwrap()
    .join(format!("{SERVICE_NAME}.zip"))
}

fn main() {
  let big = std::env::var("CORTEX_TORTURE_BIG").is_ok();
  let cap: usize = if big {
    2 * 1024 * 1024 * 1024
  } else {
    1024 * 1024
  };
  // The cap is config-driven; set it before anything reads `config()`.
  std::env::set_var("CORTEX_DISPATCHER__MAX_RESULT_BYTES", cap.to_string());

  let accept_filler = if big { 1900 * 1024 * 1024 } else { cap * 3 / 4 }; // under the cap
  let reject_gb: usize = std::env::var("CORTEX_TORTURE_REJECT_GB")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(10);
  let reject_filler = if big {
    reject_gb * 1024 * 1024 * 1024
  } else {
    cap * 3
  }; // over the cap
  let barrage_tasks = 20;

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
      description: "torture echo".into(),
    })
    .expect("add service");
  let service = Service::find_by_name(SERVICE_NAME, &mut backend.connection).unwrap();

  // Stage: M small barrage tasks + the cap accept/reject tasks.
  let mut barrage_entries = Vec::new();
  for i in 0..barrage_tasks {
    barrage_entries.push(stage_task(
      &mut backend,
      &corpus,
      &service,
      &format!("barrage{i}"),
      4096,
    ));
  }
  let accept_entry = stage_task(&mut backend, &corpus, &service, "cap_accept", accept_filler);
  let reject_entry = stage_task(&mut backend, &corpus, &service, "cap_reject", reject_filler);

  println!(
    "[torture] cap {cap} bytes; barrage {barrage_tasks} tasks; cap accept {accept_filler}B / reject {reject_filler}B"
  );

  // Start the real dispatcher + a real EchoWorker (both perpetual).
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
  thread::spawn(move || {
    EchoWorker {
      service: SERVICE_NAME.to_string(),
      version: 0.1,
      message_size: 100_000,
      source: format!("tcp://127.0.0.1:{SOURCE_PORT}"),
      sink: format!("tcp://127.0.0.1:{RESULT_PORT}"),
      identity: "torture-echo-worker".to_string(),
    }
    .start(None)
    .ok();
  });

  // --- Test 1: barrage of malformed replies, injected concurrently
  // -------------------------------- A raw PUSH that floods the sink with envelopes that are *too
  // short* (would desync framing without the RCVMORE hardening) or carry bogus task ids. If the
  // sink desyncs, it eats a real worker reply and that task never finalizes → the drain below
  // times out.
  thread::spawn(move || {
    let ctx = zmq::Context::new();
    let push = ctx.socket(zmq::PUSH).unwrap();
    push
      .connect(&format!("tcp://127.0.0.1:{RESULT_PORT}"))
      .unwrap();
    for n in 0..200_000u64 {
      // Cycle the malformed shapes.
      let frames: Vec<&[u8]> = match n % 5 {
        0 => vec![b""],                                               // single empty frame
        1 => vec![b"badworker"],                                      // id only
        2 => vec![b"badworker", SERVICE_NAME.as_bytes()],             // id + service, no taskid
        3 => vec![b"badworker", SERVICE_NAME.as_bytes(), b"-999999"], // bogus taskid, no data
        _ => vec![
          b"badworker",
          SERVICE_NAME.as_bytes(),
          b"-999999",
          b"junkjunkjunk",
        ], // bogus + data
      };
      if push.send_multipart(&frames, 0).is_err() {
        break;
      }
      if n % 1000 == 0 {
        thread::sleep(Duration::from_millis(1));
      }
    }
  });

  // Drain: every barrage task must finalize despite the flood (proves no desync / crash).
  let deadline = Duration::from_secs(if big { 600 } else { 60 });
  let start = Instant::now();
  let all_terminal = |conn: &mut diesel::PgConnection, entries: &[String]| {
    entries.iter().all(|e| status_of(conn, e, service.id) < 0)
  };
  let mut barrage_ok = false;
  while start.elapsed() < deadline {
    if all_terminal(&mut backend.connection, &barrage_entries) {
      barrage_ok = true;
      break;
    }
    thread::sleep(Duration::from_millis(200));
  }
  assert!(
    barrage_ok,
    "BARRAGE: not all {barrage_tasks} real tasks finalized under the malformed-reply flood (sink desync/crash?)"
  );
  println!("✓ barrage: all {barrage_tasks} real tasks finalized despite the malformed-reply flood");

  // --- Test 2: the hard size cap
  // ------------------------------------------------------------------ Wait for both cap tasks to
  // reach a terminal status.
  while start.elapsed() < deadline {
    if status_of(&mut backend.connection, &accept_entry, service.id) < 0
      && status_of(&mut backend.connection, &reject_entry, service.id) < 0
    {
      break;
    }
    thread::sleep(Duration::from_millis(200));
  }

  // Under-cap reply: accepted + written to disk, task terminal (the cortex.log → NoProblem).
  let accept_result = result_path(&accept_entry);
  let accept_status = status_of(&mut backend.connection, &accept_entry, service.id);
  let accept_written = accept_result.metadata().map(|m| m.len()).unwrap_or(0);
  assert_eq!(
    accept_status,
    TaskStatus::NoProblem.raw(),
    "ACCEPT: under-cap result should be NoProblem"
  );
  assert!(
    accept_written >= accept_filler as u64,
    "ACCEPT: under-cap result must be written to disk ({accept_written} bytes < {accept_filler})"
  );
  println!("✓ cap: under-cap result accepted + written ({accept_written} bytes), task NoProblem");

  // Over-cap reply: rejected (task Invalid), and no oversized file left on disk.
  let reject_result = result_path(&reject_entry);
  let reject_status = status_of(&mut backend.connection, &reject_entry, service.id);
  let reject_left = reject_result.metadata().map(|m| m.len()).unwrap_or(0);
  assert_eq!(
    reject_status,
    TaskStatus::Invalid.raw(),
    "REJECT: over-cap result should be Invalid"
  );
  assert!(
    reject_left <= cap as u64,
    "REJECT: an oversized result file ({reject_left} bytes) was left on disk past the {cap}-byte cap"
  );
  println!(
    "✓ cap: over-cap result rejected (Invalid), no oversized file left ({reject_left} bytes)"
  );

  // Cleanup the (potentially huge) staged payloads + results.
  fs::remove_dir_all(SCRATCH).ok();
  eprintln!("dispatcher_torture_test: all cases passed");
  unsafe { libc::_exit(0) }
}
