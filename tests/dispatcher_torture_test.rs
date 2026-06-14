// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Dispatcher robustness torture tests against the *real* `TaskManager` + `EchoWorker`:
//!   1. A **malformed-reply barrage**: a raw `PUSH` floods the **sink** with bad/empty/truncated
//!      replies + bogus task ids (no frames, id-only, no taskid, unknown id), injected concurrently
//!      with real work — exercising the `[identity, service, taskid, …]` envelope hardening. It
//!      asserts both that every real task still **finalizes** (the sink does not desync / strand a
//!      reply) and — the **data-integrity** guard — that every *accepted* result is a **byte-exact
//!      echo of its source** (no malformed message was ever accepted/written for a real task).
//!
//! A concurrent **ventilator request-framing flood** (D-4) runs alongside test 1: a raw `DEALER`
//! floods the ventilator ROUTER with empty-service, over-long, and unknown-service requests. Run
//! together with the sink barrage it is the regression gate for **KNOWN_ISSUES D-12**: the
//! unknown-service *mock-replies* steady the worker so its results interleave 1:1 with the sink
//! barrage, which used to expose a sink desync — a 3-frame `[identity, service, taskid]` reply with
//! no data made the sink read past the message boundary and swallow the *next* real worker result,
//! stranding its task `Queued`. Fixed by the taskid-frame `RCVMORE` guard in `sink.rs`. Knobs:
//! `CORTEX_TORTURE_{SINK,VENT}_FLOOD=0`, `CORTEX_TORTURE_VENT_SHAPE` (`skip`|`mock`|`mixed`),
//! `CORTEX_TORTURE_DEADLINE_SECS`, `CORTEX_WORKER_THROTTLE_SECS`.
//!
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

  // On an empty / mock reply (`taskid 0` — momentary-empty-queue, backpressure, or unknown-service)
  // the `pericortex` worker sends an empty reply and then *naps* `CORTEX_WORKER_THROTTLE_SECS`
  // (default 60). The D-12 straggler was originally *suspected* to be this nap; the investigation
  // (2026-06-14, see KNOWN_ISSUES D-12) instead found a sink framing desync (a 3-frame reply with
  // no data swallowing the next real result — now fixed). The throttle is set *short* here only
  // so a worker that naps once the corpus drains doesn't dominate the tail of the drain window;
  // set `CORTEX_WORKER_THROTTLE_SECS=60` to mimic the slower production nap. Must be set before
  // the worker thread reads it. (Throttle is configurable since pericortex 0.2.5, OPEN_QUESTIONS
  // #14.)
  if std::env::var("CORTEX_WORKER_THROTTLE_SECS").is_err() {
    std::env::set_var("CORTEX_WORKER_THROTTLE_SECS", "1");
  }
  let worker_throttle = std::env::var("CORTEX_WORKER_THROTTLE_SECS").unwrap();

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
    "[torture] cap {cap} bytes; barrage {barrage_tasks} tasks; cap accept {accept_filler}B / reject {reject_filler}B; worker_throttle {worker_throttle}s"
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
  let sink_flood = std::env::var("CORTEX_TORTURE_SINK_FLOOD")
    .map(|v| v != "0")
    .unwrap_or(true);
  let vent_flood = std::env::var("CORTEX_TORTURE_VENT_FLOOD")
    .map(|v| v != "0")
    .unwrap_or(true);
  eprintln!("[torture] sink_flood={sink_flood} vent_flood={vent_flood}");
  if sink_flood {
    thread::spawn(move || {
      let ctx = zmq::Context::new();
      let push = ctx.socket(zmq::PUSH).unwrap();
      push
        .connect(&format!("tcp://127.0.0.1:{RESULT_PORT}"))
        .unwrap();
      // A heavy but bounded barrage. (Discard logging is now rate-limited — KNOWN_ISSUES D-11 — so
      // the flood no longer self-throttles the sink via per-message stderr; a one-off 200k run
      // drained in ~4 s vs. stranding tasks before that fix. Kept moderate here so the
      // framing/integrity gate stays reliable rather than racing the worker's empty-queue
      // throttle under a max-rate flood.)
      for n in 0..20_000u64 {
        // Cycle the malformed shapes. Shape 3 — `[identity, service, taskid]` with **no data
        // frame** — is the one that, before the taskid `RCVMORE` guard (KNOWN_ISSUES D-12), made
        // the sink read past the message boundary and swallow the *next* real reply.
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
  }

  // --- Test 1b: ventilator request-framing flood (D-4 hardening), injected concurrently
  // -------------------------------- A raw DEALER floods the ventilator ROUTER with malformed
  // *requests*: empty service (the "3 adjacent empty messages" root cause), and over-long /
  // unknown-service requests (exercising the RCVMORE trailing-frame drain + unknown-service
  // mock-reply). The ROUTER prepends this DEALER's identity, so each send arrives as
  // `[identity, <frames>]`. **Deliberately never sends the real `SERVICE_NAME` in a dispatchable
  // request** — a valid over-long request would have the ventilator (correctly) lease a real task
  // to this non-responding peer, stranding it for the reaper (a "worker died mid-task" case, not a
  // framing fault). Restricting to skip / mock-reply shapes isolates the D-12 question: does the
  // flood perturb the *real* worker into an empty-queue nap (the suspected straggler cause), or
  // desync the ventilator (a framing fault)? If the ventilator desyncs, the real worker's request
  // gets a wrong/empty reply and its task never finalizes → the drain below times out.
  if vent_flood {
    thread::spawn(move || {
      let ctx = zmq::Context::new();
      let dealer = ctx.socket(zmq::DEALER).unwrap();
      dealer.set_identity(b"torture-flood-peer").ok();
      if dealer
        .connect(&format!("tcp://127.0.0.1:{SOURCE_PORT}"))
        .is_err()
      {
        return;
      }
      // Shape selector lets the investigation isolate *skip* (no reply emitted) from *mock-reply*
      // (the ventilator answers the bad request) paths. "mixed" (default) cycles all four.
      let shape = std::env::var("CORTEX_TORTURE_VENT_SHAPE").unwrap_or_else(|_| "mixed".into());
      for n in 0..20_000u64 {
        let sel = match shape.as_str() {
          "skip" => n % 2,     // 0,1 -> empty-service skips only
          "mock" => 2 + n % 2, // 2,3 -> unknown-service mock-replies only
          _ => n % 4,
        };
        let ok = match sel {
          0 => dealer.send_multipart([b"".as_ref()], 0), // empty service -> skip
          1 => dealer.send_multipart([b"".as_ref(), b"".as_ref()], 0), // adjacent empties -> skip
          2 => dealer.send_multipart([b"no_such_service".as_ref()], 0), /* unknown service -> */
          // mock-reply
          _ => dealer.send_multipart(
            [
              b"no_such_service".as_ref(),
              b"trailing1".as_ref(),
              b"trailing2".as_ref(),
            ],
            0,
          ), // over-long unknown -> drain trailing + mock-reply
        };
        if ok.is_err() {
          break;
        }
        if n % 1000 == 0 {
          thread::sleep(Duration::from_millis(1));
        }
      }
    });
  }

  // Drain: every barrage task must finalize despite the malformed-reply flood (proves the sink does
  // not desync / crash on a short/empty/bogus reply).
  let deadline = Duration::from_secs(
    std::env::var("CORTEX_TORTURE_DEADLINE_SECS")
      .ok()
      .and_then(|v| v.parse().ok())
      .unwrap_or(if big { 600 } else { 60 }),
  );
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
  if !barrage_ok {
    let mut todo = 0;
    let mut queued = 0;
    let mut terminal = 0;
    let mut stuck = Vec::new();
    for e in &barrage_entries {
      match status_of(&mut backend.connection, e, service.id) {
        0 => todo += 1,
        s if s > 0 => {
          queued += 1;
          stuck.push((e.clone(), s));
        },
        _ => terminal += 1,
      }
    }
    eprintln!("[diag] barrage at deadline: terminal={terminal} todo={todo} queued={queued}");
    for (e, s) in &stuck {
      let result_exists = result_path(e).exists();
      let tid = tasks::table
        .filter(tasks::entry.eq(e))
        .filter(tasks::service_id.eq(service.id))
        .select(tasks::id)
        .first::<i64>(&mut backend.connection)
        .unwrap_or(-1);
      eprintln!(
        "[diag] stuck Queued taskid={tid} status={s} result_on_disk={result_exists} entry={e}"
      );
    }
  }
  assert!(
    barrage_ok,
    "BARRAGE: not all {barrage_tasks} real tasks finalized under the malformed-reply flood (sink desync/crash?)"
  );
  println!(
    "✓ barrage: all {barrage_tasks} real tasks finalized despite the malformed sink-reply flood"
  );

  // --- Test 1c: DATA INTEGRITY of accepted results
  // ------------------------------------------------ Finalizing is necessary but not sufficient:
  // a sink framing desync under the flood could splice a garbage reply's bytes into a real task's
  // result. The EchoWorker echoes the source verbatim (`convert` = `File::open`), so each
  // *accepted* result must be a **byte-for-byte copy of its source** and must parse to
  // `NoProblem`. A single mismatch means a malformed message was accepted.
  for entry in &barrage_entries {
    let status = status_of(&mut backend.connection, entry, service.id);
    assert_eq!(
      status,
      TaskStatus::NoProblem.raw(),
      "INTEGRITY: real task finalized to {status}, not NoProblem — a malformed reply corrupted an accepted result? ({entry})"
    );
    let source_bytes = fs::read(entry).expect("read source zip");
    let result_bytes = fs::read(result_path(entry)).expect("read result zip");
    assert_eq!(
      result_bytes, source_bytes,
      "INTEGRITY: accepted result for {entry} is not a byte-exact echo of its source — corruption from the framing flood"
    );
  }
  println!(
    "✓ integrity: all {barrage_tasks} accepted results are byte-exact echoes of their source (no malformed message accepted)"
  );

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
