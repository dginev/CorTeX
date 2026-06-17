// Copyright 2015-2025 Deyan Ginev. MIT license.
//
// Dispatcher-rationalization SPIKE (throwaway; docs/archive/DISPATCHER_RATIONALIZATION.md).
// Examines **unexpected deaths** in the async dispatcher core (owner, 2026-06-14): if the DB
// refuses connection and the finalize arm dies, or the disk dies mid-write, or a firewall blocks
// one direction — we must **try recovery, and if impossible stop the *entire* dispatcher**, never
// leaving a zombie arm leasing into a dead pipeline, and never an inconsistent durable state.
//
// The risk this targets is specific to the tokio migration: a dropped JoinHandle / swallowed error
// silently kills one arm while the rest run on. So the async core needs an explicit **supervisor**:
// a shared HALT signal that any arm trips on a fatal (after a *bounded* recovery attempt), which
// every arm observes → the whole dispatcher stops with one reason. This spike builds that harness
// and injects each catastrophe.
//
// Durable-state model: each task has a status Todo→Queued(leased)→Done(persisted). The *commit
// point* is the finalize batch writing to the "DB" (`persisted`); the Queued mark is cleared only
// at commit. So a result received but not yet persisted is still Queued = **recoverable on
// restart** — never lost.
//
// Run (FAULT ∈ none|db_transient|db_dead|io_full|net_oneway):
//   for f in none db_transient db_dead io_full net_oneway; do FAULT=$f \
//     cargo run --release --example zmq_faults; done

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use zeromq::{PullSocket, RouterSocket, Socket, SocketRecv, SocketSend, ZmqMessage};

const TODO: u8 = 0;
const QUEUED: u8 = 1;
const DONE: u8 = 2;
const RETRY_SENTINEL: u64 = u64::MAX;

fn env_usize(key: &str, default: usize) -> usize {
  std::env::var(key)
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(default)
}

/// The supervisor's shared halt signal. The first arm to trip it records the reason; every arm
/// polls `tripped()` and stops; `main` awaits `notify`.
struct Halt {
  tripped: AtomicBool,
  reason: Mutex<Option<&'static str>>,
  notify: tokio::sync::Notify,
}
impl Halt {
  fn new() -> Arc<Self> {
    Arc::new(Halt {
      tripped: AtomicBool::new(false),
      reason: Mutex::new(None),
      notify: tokio::sync::Notify::new(),
    })
  }
  fn trip(&self, reason: &'static str) {
    if !self.tripped.swap(true, Ordering::SeqCst) {
      *self.reason.lock().unwrap() = Some(reason);
      self.notify.notify_one();
    }
  }
  fn tripped(&self) -> bool { self.tripped.load(Ordering::SeqCst) }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  let fault = std::env::var("FAULT").unwrap_or_else(|_| "none".into());
  let tasks = env_usize("TASKS", 500);
  let workers = env_usize("WORKERS", 16);
  let max_in_flight = env_usize("MAX_IN_FLIGHT", 64);
  let db_retry = 4usize; // bounded reconnect budget per batch before escalating
  let io_retry = 4usize; // bounded write-retry budget before escalating
  let transient_fails = 3usize; // db_transient recovers after this many failures
  let db_batch = 64usize;

  println!("fault spike: FAULT={fault} — {workers} workers, {tasks} tasks (recover-or-halt-all)");

  let status: Arc<Mutex<HashMap<u64, u8>>> =
    Arc::new(Mutex::new((0..tasks as u64).map(|s| (s, TODO)).collect()));
  let persisted: Arc<Mutex<HashSet<u64>>> = Arc::new(Mutex::new(HashSet::new()));
  let acked = Arc::new(AtomicUsize::new(0)); // results received by the sink (in-flight cache cleared)
  let todo: Arc<Mutex<VecDeque<u64>>> = Arc::new(Mutex::new((0..tasks as u64).collect()));
  let outstanding = Arc::new(AtomicUsize::new(0)); // leased − persisted (drives backpressure + watchdog)
  let commit_attempts = Arc::new(AtomicUsize::new(0));
  let halt = Halt::new();
  let shutdown = Arc::new(AtomicBool::new(false));
  let complete = Arc::new(tokio::sync::Notify::new());
  let (fin_tx, mut fin_rx) = tokio::sync::mpsc::channel::<u64>(db_batch * 4);

  // FINALIZE arm: batch-commit to the "DB"; bounded reconnect retry; escalate to halt if the DB
  // stays down (the owner's "finalize arm dies" case). A failed batch is *not* committed, so its
  // tasks stay Queued → recoverable.
  let finalize = {
    let (status, persisted, halt, complete, fault, commit_attempts) = (
      status.clone(),
      persisted.clone(),
      halt.clone(),
      complete.clone(),
      fault.clone(),
      commit_attempts.clone(),
    );
    tokio::spawn(async move {
      while let Some(first) = fin_rx.recv().await {
        if halt.tripped() {
          break;
        }
        let mut batch = vec![first];
        while batch.len() < db_batch {
          match fin_rx.try_recv() {
            Ok(s) => batch.push(s),
            Err(_) => break,
          }
        }
        let mut tries = 0usize;
        loop {
          // Mock the DB commit; inject connection refusal.
          let n = commit_attempts.fetch_add(1, Ordering::Relaxed);
          let refused = match fault.as_str() {
            "db_dead" => true,
            "db_transient" => n < transient_fails,
            _ => false,
          };
          tokio::time::sleep(Duration::from_millis(5)).await;
          if !refused {
            let mut p = persisted.lock().unwrap();
            let mut s = status.lock().unwrap();
            for seq in &batch {
              p.insert(*seq);
              s.insert(*seq, DONE);
            }
            if p.len() >= tasks {
              complete.notify_one();
            }
            break;
          }
          tries += 1;
          if tries > db_retry {
            halt.trip("DB connection refused — finalize cannot persist after bounded retries");
            return;
          }
          tokio::time::sleep(Duration::from_millis(20 * tries as u64)).await; // backoff
        }
      }
    })
  };

  // SINK arm: PULL results. Inject the I/O catastrophe (the /data write) and the one-directional
  // transport block (results received but never forwarded). Clears the in-flight cache on receipt;
  // the durable Queued mark is *not* touched here (only finalize commits).
  let mut sink = PullSocket::new();
  let sink_ep = sink.bind("tcp://127.0.0.1:0").await?.to_string();
  let sink_task = {
    let (halt, acked, outstanding, fault, fin_tx) = (
      halt.clone(),
      acked.clone(),
      outstanding.clone(),
      fault.clone(),
      fin_tx.clone(),
    );
    tokio::spawn(async move {
      loop {
        if halt.tripped() {
          break;
        }
        let msg = match sink.recv().await {
          Ok(m) => m,
          Err(_) => break,
        };
        let seq = u64::from_le_bytes(msg.get(0).unwrap()[0..8].try_into().unwrap());
        // (I/O) the blocking /data archive write — inject ENOSPC / disk death.
        if fault == "io_full" {
          let mut tries = 0usize;
          loop {
            tries += 1;
            tokio::time::sleep(Duration::from_millis(2)).await;
            if tries > io_retry {
              halt.trip("disk write failed (full/dead) after bounded retries");
              return;
            }
          }
        }
        // (NET) one-directional block: the result arrived but the onward path is firewalled — drop
        // it, so nothing is acked/forwarded and the watchdog must catch the stall.
        if fault == "net_oneway" {
          continue;
        }
        acked.fetch_add(1, Ordering::Relaxed);
        outstanding.fetch_sub(1, Ordering::Relaxed);
        if fin_tx.send(seq).await.is_err() {
          break;
        }
      }
    })
  };

  // VENTILATOR arm: ROUTER. Leases (Todo→Queued) under a backpressure cap; on a dead-peer send,
  // re-queues at once.
  let mut router = RouterSocket::new();
  let router_ep = router.bind("tcp://127.0.0.1:0").await?.to_string();
  let ventilator = {
    let (todo, status, outstanding, halt) = (
      todo.clone(),
      status.clone(),
      outstanding.clone(),
      halt.clone(),
    );
    tokio::spawn(async move {
      while let Ok(req) = router.recv().await {
        if halt.tripped() {
          break;
        }
        let identity = req.get(0).cloned().unwrap();
        let mut reply = ZmqMessage::from(identity.to_vec());
        let next = if outstanding.load(Ordering::Relaxed) >= max_in_flight {
          None // backpressure
        } else {
          todo.lock().unwrap().pop_front()
        };
        match next {
          Some(seq) => {
            status.lock().unwrap().insert(seq, QUEUED);
            outstanding.fetch_add(1, Ordering::Relaxed);
            let mut header = vec![0u8; 8];
            header[0..8].copy_from_slice(&seq.to_le_bytes());
            reply.push_back(Bytes::from(header));
            reply.push_back(Bytes::from(vec![0u8; 4096]));
            if router.send(reply).await.is_err() {
              status.lock().unwrap().insert(seq, TODO);
              todo.lock().unwrap().push_front(seq);
              outstanding.fetch_sub(1, Ordering::Relaxed);
            }
          },
          None => {
            let mut header = vec![0u8; 8];
            header[0..8].copy_from_slice(&RETRY_SENTINEL.to_le_bytes());
            reply.push_back(Bytes::from(header));
            let _ = router.send(reply).await;
          },
        }
      }
    })
  };

  // WATCHDOG arm: the liveness check the per-task timeout can't provide — if work is outstanding
  // but *no* task gets persisted for a while, a one-directional transport block (or a wedged
  // finalize) is stalling everything → trip the halt. This is the new requirement surfaced by the
  // firewall case.
  let watchdog = {
    let (persisted, outstanding, halt, complete) = (
      persisted.clone(),
      outstanding.clone(),
      halt.clone(),
      complete.clone(),
    );
    tokio::spawn(async move {
      let mut last = 0usize;
      let mut stalled_ticks = 0;
      loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if halt.tripped() || persisted.lock().unwrap().len() >= tasks {
          let _ = &complete;
          break;
        }
        let now = persisted.lock().unwrap().len();
        if now == last && outstanding.load(Ordering::Relaxed) > 0 {
          stalled_ticks += 1;
          if stalled_ticks >= 8 {
            // ~4 s of zero progress with work outstanding
            halt
              .trip("no progress while work outstanding — one-directional transport / wedged arm");
            return;
          }
        } else {
          stalled_ticks = 0;
        }
        last = now;
      }
    })
  };

  let start = Instant::now();

  // Workers (libzmq DEALER + PUSH).
  let ctx = Arc::new(zmq::Context::new());
  let mut handles = Vec::new();
  for w in 0..workers {
    let (ctx, router_ep, sink_ep, shutdown) = (
      ctx.clone(),
      router_ep.clone(),
      sink_ep.clone(),
      shutdown.clone(),
    );
    handles.push(thread::spawn(move || {
      let dealer = ctx.socket(zmq::DEALER).unwrap();
      dealer.set_identity(format!("w{w}").as_bytes()).unwrap();
      dealer.set_rcvtimeo(300).unwrap();
      dealer.set_sndtimeo(300).unwrap();
      dealer.connect(&router_ep).unwrap();
      let push = ctx.socket(zmq::PUSH).unwrap();
      push.set_sndtimeo(300).unwrap();
      push.connect(&sink_ep).unwrap();
      while !shutdown.load(Ordering::Relaxed) {
        if dealer.send_multipart([b"rq".as_ref()], 0).is_err() {
          continue;
        }
        let src = match dealer.recv_multipart(0) {
          Ok(m) => m,
          Err(_) => continue,
        };
        let seq = u64::from_le_bytes(src[0][0..8].try_into().unwrap());
        if seq == RETRY_SENTINEL {
          thread::sleep(Duration::from_millis(10));
          continue;
        }
        let mut header = vec![0u8; 8];
        header[0..8].copy_from_slice(&seq.to_le_bytes());
        let _ = push.send_multipart([header.as_slice(), &[0u8; 1024]], 0);
      }
    }));
  }

  // SUPERVISOR: wait for normal completion OR any arm tripping the halt; then stop *everything*.
  let outcome = tokio::select! {
    _ = complete.notified() => "completed",
    _ = halt.notify.notified() => "halted",
  };
  shutdown.store(true, Ordering::Relaxed);
  ventilator.abort();
  sink_task.abort();
  finalize.abort();
  watchdog.abort();
  for h in handles {
    let _ = h.join();
  }
  let secs = start.elapsed().as_secs_f64();

  // Consistency audit: nothing acked may be LOST — every task is Todo, Queued (recoverable), or
  // Done (persisted), and Done == persisted exactly. A result received-but-unpersisted is still
  // Queued.
  let s = status.lock().unwrap();
  let p = persisted.lock().unwrap();
  let done_in_status = s.values().filter(|&&v| v == DONE).count();
  let lost = s.iter().filter(|&(seq, &v)| v != DONE && p.contains(seq)).count() // persisted but not Done
    + p.iter().filter(|seq| !s.contains_key(seq)).count(); // persisted but unknown task
  let recoverable = s.values().filter(|&&v| v == QUEUED).count();
  let reason = halt.reason.lock().unwrap().unwrap_or("(none)");

  println!(
    "  outcome={outcome} in {:.1}s; persisted {}/{tasks}, queued(recoverable) {recoverable}, \
     acked {}; halt reason: {}",
    secs,
    p.len(),
    acked.load(Ordering::Relaxed),
    if outcome == "halted" { reason } else { "—" },
  );
  let consistent = lost == 0 && done_in_status == p.len();
  let arms_stopped = ventilator.is_finished()
    && sink_task.is_finished()
    && finalize.is_finished()
    && watchdog.is_finished();
  match outcome {
    "completed" => {
      println!("  ✓ no fault → all {tasks} tasks persisted exactly once, state consistent")
    },
    _ if consistent && arms_stopped => println!(
      "  ✓ FATAL HANDLED: every arm stopped (no zombie), durable state consistent — 0 tasks lost, \
       {recoverable} left Queued for restart recovery"
    ),
    _ => println!("  ✗ INCONSISTENT/zombie: lost={lost}, arms_stopped={arms_stopped}"),
  }
  Ok(())
}
