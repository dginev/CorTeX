// Copyright 2015-2026 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! The sink accepts a result whose lease expired, unless a newer attempt's result already landed
//! (KNOWN_ISSUES D-26), end to end over ZMQ: a dead-lettered task's late result, a re-queued
//! task's, an older attempt's while the newer is in flight, then the newer one's (which replaces
//! it) — and an older attempt's after that, which is dropped.
//!
//! Every fixture lives under a throwaway `/tmp/cortex_late_result_*` root and the only rows are a
//! test-database service (for the service cache) and its worker metadata.

use cortex::backend::{self, build_pool, test_db_address};
use cortex::dispatcher::server::{InFlightSet, SandboxCache, ServiceCache};
use cortex::dispatcher::sink::Sink;
use cortex::helpers::{TaskProgress, TaskReport, random_mark};
use cortex::models::{NewService, Service, Task, start_metadata_writer};
use cortex::schema::services;
use diesel::prelude::*;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::sync_channel;
use std::time::Duration;

/// Sink port (distinct from every other dispatcher test's ports), below Linux's ephemeral range
/// (32768–60999), so no outgoing connection on a busy host can hold it (KNOWN_ISSUES W-8).
const SINK_PORT: usize = 31696;

#[test]
fn late_results_are_accepted_unless_a_newer_attempt_finished() {
  let service_name = format!("late_result_svc_{}", random_mark());
  let mut db = backend::testdb();
  db.add(&NewService {
    name: service_name.clone(),
    version: 0.1,
    inputformat: "tex".into(),
    outputformat: "html".into(),
    inputconverter: Some("import".into()),
    complex: true,
    description: "late result sink case".into(),
  })
  .expect("add service");
  let service = Service::find_by_name(&service_name, &mut db.connection).expect("service");

  let root = PathBuf::from(format!("/tmp/cortex_late_result_{}", std::process::id()));
  let corpus_id = i32::MAX - 3;
  let progress = |id: i64, workers: &[&str], created_at: i64| {
    let doc = root.join(format!("doc{id}"));
    std::fs::create_dir_all(&doc).expect("entry dir");
    TaskProgress {
      run_start: 0,
      last_cause: Vec::new(),
      task: Task {
        id,
        service_id: service.id,
        corpus_id,
        status: 1,
        entry: doc
          .join(format!("doc{id}.zip"))
          .to_string_lossy()
          .into_owned(),
      },
      created_at,
      retries: workers.len() as i64 - 1,
      lease_timeout_seconds: 240,
      attempt_workers: workers.iter().map(|w| w.to_string()).collect(),
    }
  };
  let base = i64::from(i32::MAX) + 100;
  let (dead, requeued, releasd, sentinel) = (base + 1, base + 2, base + 3, base + 4);
  let empty_late = base + 5;
  let (retried, crashed) = (base + 6, base + 7);
  let last_retry = base + 8;
  let now = chrono::Utc::now().timestamp();
  let in_flight = Arc::new(InFlightSet::new());
  // `dead`: three attempts, all expired → dead-lettered. `requeued`: one attempt, expired →
  // re-queued. Both leave the in-flight set for the late-result window.
  in_flight.insert(progress(dead, &["w-dead-0", "w-dead-1", "w-dead-2"], 0));
  in_flight.insert(progress(requeued, &["w-req-0"], 0));
  // `empty_late`: re-queued too; its late "result" will be an empty archive.
  in_flight.insert(progress(empty_late, &["w-empty-0"], 0));
  assert_eq!(in_flight.take_expired().len(), 3);
  // `releasd`: attempt 0 expired, attempt 1 (w-rel-1) leased and in flight.
  in_flight.insert(progress(releasd, &["w-rel-0", "w-rel-1"], now));
  in_flight.insert(progress(sentinel, &["w-sentinel"], now));
  // `retried` and `crashed`: in flight; the worker harness will report a retryable timeout for
  // one and a crash (final on its first occurrence) for the other.
  in_flight.insert(progress(retried, &["w-retry-0"], now));
  in_flight.insert(progress(crashed, &["w-crash-0"], now));
  // `last_retry`: its third and last attempt in flight; its retryable timeout is final.
  in_flight.insert(progress(
    last_retry,
    &["w-last-0", "w-last-1", "w-last-2"],
    now,
  ));
  assert_eq!((in_flight.len(), in_flight.settling_len()), (5, 3));

  let sandboxes = Arc::new(SandboxCache::new());
  sandboxes.insert(corpus_id, None);
  let services_cache = Arc::new(ServiceCache::new());
  services_cache.insert(service_name.clone(), Some(service.clone()));
  let (done_tx, done_rx) = sync_channel::<TaskReport>(16);
  let metadata = start_metadata_writer(build_pool(test_db_address(), 1));
  let dispatch_complete = Arc::new(AtomicBool::new(false));
  let (sink_in_flight, sink_complete) = (Arc::clone(&in_flight), Arc::clone(&dispatch_complete));
  let sink = std::thread::spawn(move || {
    Sink {
      port: SINK_PORT,
      queue_size: 8,
      message_size: 100_000,
      backend_address: test_db_address().to_string(),
      metadata,
    }
    .start(
      &services_cache,
      &sandboxes,
      &sink_in_flight,
      &done_tx,
      None,
      &sink_complete,
    )
    .map_err(|e| e.to_string())
  });

  let ctx = zmq::Context::new();
  let push = ctx.socket(zmq::PUSH).expect("push socket");
  // Undelivered messages must not block the context's drop (and the test) if the sink failed.
  push.set_linger(0).expect("linger");
  push
    .connect(&format!("tcp://127.0.0.1:{SINK_PORT}"))
    .expect("connect to sink");
  // A result archive whose `cortex.log` is `log`.
  let log_archive = |log: &str| {
    let mut zipped = std::io::Cursor::new(Vec::new());
    let mut zw = zip::ZipWriter::new(&mut zipped);
    zw.start_file("cortex.log", zip::write::SimpleFileOptions::default())
      .expect("zip entry");
    std::io::Write::write_all(&mut zw, log.as_bytes()).expect("zip write");
    zw.finish().expect("zip finish");
    zipped.into_inner()
  };
  // A result archive whose conversion status is `status` (CorTeX status `-(status + 1)`).
  let archive = |status: u8| log_archive(&format!("Status:conversion:{status}\n"));
  let send = |worker: &str, id: i64, data: &[u8]| {
    push
      .send_multipart(
        [
          worker.as_bytes(),
          service_name.as_bytes(),
          id.to_string().as_bytes(),
          data,
        ],
        0,
      )
      .expect("send result");
  };
  send("w-dead-2", dead, &archive(1)); // accepted: replaces the never_completed dead-letter
  send("w-req-0", requeued, &archive(1)); // accepted: the queued retry will not be leased
  send("w-empty-0", empty_late, &[]); // empty: claims nothing, the retry stays leasable
  send("w-rel-0", releasd, &archive(2)); // accepted: an older attempt, the newer in flight
  send("w-rel-1", releasd, &archive(0)); // accepted: the newer attempt replaces it
  send("w-rel-0", releasd, &archive(2)); // dropped: superseded by the newer attempt's result
  send("w-rel-1", releasd, b"not a zip"); // unreadable: dropped, the published archive kept
  send("w-unknown", base + 99, &archive(1)); // dropped: no such task
  // The harness's reports: a retryable timeout (back to the retry budget, not recorded) and a
  // crash (recorded at once).
  send(
    "w-retry-0",
    retried,
    &log_archive(
      "Warning:x:y partial\nInfo:cortex:retryable timeout:wallclock\nFatal:timeout:wallclock \
       watchdog\nStatus:conversion:3\n",
    ),
  );
  send(
    "w-crash-0",
    crashed,
    &log_archive("Fatal:crash:SIGSEGV killed by signal 11\nStatus:conversion:3\n"),
  );
  // The last attempt's retryable timeout: published and reported as the stand-in (attempt 0's
  // late good result, sent once the stand-in is in, replaces it — below).
  send(
    "w-last-2",
    last_retry,
    &log_archive(
      "Info:cortex:retryable timeout:wallclock\nFatal:timeout:wallclock watchdog\nStatus:\
       conversion:3\n",
    ),
  );
  send("w-sentinel", sentinel, &archive(1)); // accepted: received after all the above

  let mut reported: BTreeMap<i64, usize> = BTreeMap::new();
  let mut releasd_statuses = Vec::new();
  let mut last_retry_reports = Vec::new();
  let mut stand_in_archive_log = None;
  for round in 0..8 {
    if round == 7 {
      // The stand-in is in (all seven earlier results are): now attempt 0's late good result.
      send("w-last-0", last_retry, &archive(0));
    }
    let report = done_rx
      .recv_timeout(Duration::from_secs(30))
      .expect("a report");
    *reported.entry(report.task.id).or_default() += 1;
    if report.task.id == last_retry {
      if report.stand_in {
        let path = root
          .join(format!("doc{last_retry}"))
          .join(format!("{service_name}.zip"));
        stand_in_archive_log = std::fs::File::open(path).ok().and_then(|file| {
          let mut archive = zip::ZipArchive::new(file).ok()?;
          let mut log = String::new();
          std::io::Read::read_to_string(&mut archive.by_name("cortex.log").ok()?, &mut log).ok()?;
          Some(log)
        });
      }
      last_retry_reports.push((report.stand_in, report.status.raw()));
    }
    if report.task.id == releasd {
      releasd_statuses.push(report.status.raw());
    }
  }
  let extra = done_rx.recv_timeout(Duration::from_secs(2)).ok();
  dispatch_complete.store(true, Ordering::Release);
  let sink_outcome = sink.join().expect("sink thread");
  let held_after = (in_flight.len(), in_flight.settling_len());
  // While `retried` is retried, its cause archive is not published.
  let retried_archive_present = root
    .join(format!("doc{retried}"))
    .join(format!("{service_name}.zip"))
    .exists();
  // The retryable timeout left `retried` in flight, due at the next reap, with its cause.
  let due: Vec<TaskProgress> = in_flight.take_expired();
  let retried_cause: Vec<String> = due
    .iter()
    .filter(|p| p.task.id == retried)
    .flat_map(|p| {
      use cortex::models::LogRecord;
      p.last_cause
        .iter()
        .map(|m| format!("{}:{}", m.category(), m.what()))
    })
    .collect();
  // The published archive of `releasd` is the newer attempt's (status 0), intact.
  let published = cortex::helpers::generate_report(
    in_flight.task(releasd).expect("held"),
    &root
      .join(format!("doc{releasd}"))
      .join(format!("{service_name}.zip")),
  )
  .map(|report| report.status.raw());
  // The empty result claimed nothing: `empty_late`'s queued retry is still leased.
  let mut retry = progress(empty_late, &["w-empty-0", "w-empty-1"], now);
  retry.retries = 1;
  let retry_leased = in_flight.lease(retry);

  let _ = diesel::delete(services::table.filter(services::name.eq(&service_name)))
    .execute(&mut db.connection);
  let _ = std::fs::remove_dir_all(&root);

  assert_eq!(
    reported,
    BTreeMap::from([
      (dead, 1),
      (requeued, 1),
      (releasd, 2),
      (crashed, 1),
      (last_retry, 2),
      (sentinel, 1)
    ]),
    "the dead-lettered, re-queued and re-leased tasks' late results are accepted (the re-leased \
     one's older attempt, then its newer one); the crash is recorded, the timeout is not"
  );
  assert_eq!(
    retried_cause,
    vec!["x:y".to_string(), "timeout:wallclock".to_string()],
    "the retryable timeout goes back to the retry budget, its cause (untagged) kept"
  );
  assert!(
    !retried_archive_present,
    "a retried task's cause archive is not published"
  );
  assert_eq!(
    last_retry_reports,
    vec![(true, -4), (false, -1)],
    "the last attempt's cause is a stand-in Fatal; attempt 0's good result replaces it"
  );
  assert!(
    stand_in_archive_log
      .as_deref()
      .is_some_and(|log| log.contains("Fatal:timeout:wallclock")),
    "the stand-in's own archive is published with it: {stand_in_archive_log:?}"
  );
  assert!(
    extra.is_none(),
    "the superseded and the unknown results are dropped, got {:?}",
    extra.map(|r| r.task.id)
  );
  assert_eq!(sink_outcome, Ok(()), "the sink drains and stops cleanly");
  assert_eq!(
    releasd_statuses,
    vec![-3, -1],
    "the older attempt's result (Error), then the newer one's (NoProblem), in arrival order"
  );
  assert_eq!(
    published,
    Some(-1),
    "the unreadable later result did not replace the published archive"
  );
  assert!(
    retry_leased,
    "an empty late result must not finish the task (its retry is leased)"
  );
  assert_eq!(
    held_after,
    (1, 5),
    "only the retried task is in flight; five tasks stay held for their window (the stand-in's \
     task too; the single-attempt crash is not held: nothing could replace it)"
  );
}
