extern crate tempfile;
extern crate zmq;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use time;

use backend::Backend;
use helpers::{TaskProgress, TaskReport};
use models::Service;

/// Persists a shared vector of reports to the Task store
pub fn mark_done_arc(
  backend: &Backend,
  reports_arc: &Arc<Mutex<Vec<TaskReport>>>,
) -> Result<bool, String>
{
  let reports = drain_shared_vec(reports_arc);
  if !reports.is_empty() {
    let request_time = time::get_time();
    let mut success = false;
    if let Err(e) = backend.mark_done(&reports) {
      println!("mark_done attempt failed: {:?}", e);
      // DB persist failed, retry
      let mut retries = 0;
      while retries < 3 {
        thread::sleep(Duration::new(2, 0)); // wait 2 seconds before retrying, in case this is latency related
        retries += 1;
        match backend.mark_done(&reports) {
          Ok(_) => {
            success = true;
            break;
          },
          Err(e) => println!("mark_done retry failed: {:?}", e),
        };
      }
    } else {
      success = true;
    }
    if !success {
      return Err(String::from(
        "Database ran away during mark_done persisting.",
      ));
    }
    let responded_time = time::get_time();
    let request_duration = (responded_time - request_time).num_milliseconds();
    println!("Reporting done tasks to DB took {}ms.", request_duration);
    Ok(true)
  } else {
    Ok(false)
  }
}
/// Adds a task report to a shared report queue
pub fn push_done_queue(reports_arc: &Arc<Mutex<Vec<TaskReport>>>, report: TaskReport) {
  let mut reports = reports_arc
    .lock()
    .unwrap_or_else(|_| panic!("Failed to obtain Mutex lock in push_done_queue"));
  if reports.len() > 10_000 {
    panic!(
      "Done queue is too large: {:?} tasks. Stop the sink!",
      reports.len()
    );
  }
  reports.push(report)
}

/// Check for, remove and return any expired tasks from the progress queue
pub fn timeout_progress_tasks<S: ::std::hash::BuildHasher>(
  progress_queue_arc: &Arc<Mutex<HashMap<i64, TaskProgress, S>>>,
) -> Vec<TaskProgress> {
  let mut progress_queue = progress_queue_arc
    .lock()
    .unwrap_or_else(|_| panic!("Failed to obtain Mutex lock in timeout_progress_tasks"));
  let now = time::get_time().sec;
  let expired_keys = progress_queue
    .iter()
    .filter(|&(_, v)| v.expected_at() < now)
    .map(|(k, _)| *k)
    .collect::<Vec<_>>();
  let mut expired_tasks = Vec::new();
  for key in expired_keys {
    match progress_queue.remove(&key) {
      None => {},
      Some(task_progress) => expired_tasks.push(task_progress),
    }
  }
  expired_tasks
}

/// Pops the next task from the progress queue
pub fn pop_progress_task<S: ::std::hash::BuildHasher>(
  progress_queue_arc: &Arc<Mutex<HashMap<i64, TaskProgress, S>>>,
  taskid: i64,
) -> Option<TaskProgress>
{
  if taskid < 0 {
    // Mock ids are to be skipped
    return None;
  }
  let mut progress_queue = progress_queue_arc
    .lock()
    .unwrap_or_else(|_| panic!("Failed to obtain Mutex lock in pop_progress_task"));
  progress_queue.remove(&taskid)
}

/// Pushes a new task on the progress queue
pub fn push_progress_task<S: ::std::hash::BuildHasher>(
  progress_queue_arc: &Arc<Mutex<HashMap<i64, TaskProgress, S>>>,
  progress_task: TaskProgress,
)
{
  let mut progress_queue = progress_queue_arc
    .lock()
    .unwrap_or_else(|_| panic!("Failed to obtain Mutex lock in push_progress_task"));
  // NOTE: This constant should be adjusted if you expect a fringe of more than 10,000 jobs
  //       I am using this as a workaround for the inability to catch thread panic!() calls.
  if progress_queue.len() > 10_000 {
    panic!(
      "Progress queue is too large: {:?} tasks. Stop the ventilator!",
      progress_queue.len()
    );
  }
  progress_queue.insert(progress_task.task.id, progress_task);
}

/// Drain a `Vec` inside an `Arc<Mutex>`
pub fn drain_shared_vec<T: Clone>(vec_arc: &Arc<Mutex<Vec<T>>>) -> Vec<T> {
  let mut vec_mutex_guard = vec_arc
    .lock()
    .unwrap_or_else(|_| panic!("Failed to obtain Mutex lock in drain_shared_vec"));
  let fetched_vec: Vec<T> = (*vec_mutex_guard).drain(..).collect();
  fetched_vec
}

/// Memoized getter for a `Service` record from the backend
pub fn get_sync_service<S: ::std::hash::BuildHasher>(
  service_name: &str,
  services: &Arc<Mutex<HashMap<String, Option<Service>, S>>>,
  backend: &Backend,
) -> Option<Service>
{
  let mut services = services
    .lock()
    .unwrap_or_else(|_| panic!("Failed to obtain Mutex lock in get_sync_services"));
  services
    .entry(service_name.to_string())
    .or_insert_with(
      || match Service::find_by_name(service_name, &backend.connection) {
        Ok(s) => Some(s),
        _ => None,
      },
    )
    .clone()
}

/// Getter for a `Service` stored inside an `Arc<Mutex<HashMap>`, with no DB access
pub fn get_service<S: ::std::hash::BuildHasher>(
  service_name: &str,
  services: &Arc<Mutex<HashMap<String, Option<Service>, S>>>,
) -> Option<Service>
{
  let services = services
    .lock()
    .unwrap_or_else(|_| panic!("Failed to obtain Mutex lock in get_service"));
  match services.get(service_name) {
    None => None, // TODO: Handle errors
    Some(service) => service.clone(),
  }
}
