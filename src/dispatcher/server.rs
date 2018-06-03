extern crate tempfile;
extern crate zmq;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use time;

use backend::Backend;
use helpers::{TaskProgress, TaskReport};
use models::Service;

/// Persists a shared vector of reports to the Task store
pub fn mark_done_arc(backend: &Backend, reports_arc: &Arc<Mutex<Vec<TaskReport>>>) -> bool {
  let reports = drain_shared_vec(reports_arc);
  if !reports.is_empty() {
    let request_time = time::get_time();
    backend.mark_done(&reports).unwrap(); // TODO: error handling if DB fails
    let responded_time = time::get_time();
    let request_duration = (responded_time - request_time).num_milliseconds();
    println!("Reporting done tasks to DB took {}ms.", request_duration);
    true
  } else {
    false
  }
}
/// Adds a task report to a shared report queue
pub fn push_done_queue(reports_arc: &Arc<Mutex<Vec<TaskReport>>>, report: TaskReport) {
  let mut reports = reports_arc.lock().unwrap();
  if reports.len() > 10_000 {
    panic!(
      "Done queue is too large: {:?} tasks. Stop the sink!",
      reports.len()
    );
  }
  reports.push(report)
}

/// Check for, remove and return any expired tasks from the progress queue
pub fn timeout_progress_tasks(
  progress_queue_arc: &Arc<Mutex<HashMap<i64, TaskProgress>>>,
) -> Vec<TaskProgress> {
  let mut progress_queue = progress_queue_arc.lock().unwrap();
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
pub fn pop_progress_task(
  progress_queue_arc: &Arc<Mutex<HashMap<i64, TaskProgress>>>,
  taskid: i64,
) -> Option<TaskProgress>
{
  let mut progress_queue = progress_queue_arc.lock().unwrap();
  progress_queue.remove(&taskid)
}

/// Pushes a new task on the progress queue
pub fn push_progress_task(
  progress_queue_arc: &Arc<Mutex<HashMap<i64, TaskProgress>>>,
  progress_task: TaskProgress,
)
{
  let mut progress_queue = progress_queue_arc.lock().unwrap();
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
  let mut vec_mutex_guard = vec_arc.lock().unwrap();
  let fetched_vec: Vec<T> = (*vec_mutex_guard).drain(..).collect();
  fetched_vec
}

/// Memoized getter for a `Service` record from the backend
pub fn get_sync_service(
  service_name: &str,
  services: &Arc<Mutex<HashMap<String, Option<Service>>>>,
  backend: &Backend,
) -> Option<Service>
{
  let mut services = services.lock().unwrap();
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
pub fn get_service(
  service_name: &str,
  services: &Arc<Mutex<HashMap<String, Option<Service>>>>,
) -> Option<Service>
{
  let services = services.lock().unwrap();
  match services.get(service_name) {
    None => None, // TODO: Handle errors
    Some(service) => service.clone(),
  }
}
