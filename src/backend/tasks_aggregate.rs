//! Aggregate methods, to be used by backend
use crate::helpers::TaskStatus;
use crate::models::{Service, Task};
use crate::schema::tasks;
use diesel::result::Error;
use diesel::*;
use rand::{Rng, thread_rng};

pub(crate) fn fetch_tasks(
  connection: &mut PgConnection,
  service: &Service,
  queue_size: usize,
) -> Result<Vec<Task>, Error> {
  use crate::schema::tasks::dsl::{service_id, status};
  let mut rng = thread_rng();
  let mark: u16 = 1 + rng.r#gen::<u16>();

  let mut marked_tasks: Vec<Task> = Vec::new();
  connection.transaction::<(), Error, _>(|t_connection| {
    let tasks_for_update = tasks::table
      .for_update()
      .filter(service_id.eq(service.id))
      .filter(status.eq(TaskStatus::TODO.raw()))
      .limit(queue_size as i64)
      .load(t_connection)?;
    marked_tasks = tasks_for_update
      .into_iter()
      .map(|task| Task {
        status: i32::from(mark),
        ..task
      })
      .map(|task| task.save_changes(t_connection))
      .filter_map(Result::ok)
      .collect();
    Ok(())
  })?;
  Ok(marked_tasks)
}

pub(crate) fn clear_limbo_tasks(connection: &mut PgConnection) -> Result<usize, Error> {
  clear_limbo_tasks_except(connection, &[])
}

/// Reset every Queued (positive-`status` lease mark) task back to `TODO`, **except** the given
/// in-flight task ids. At process start (nothing in flight) pass `&[]` to recover all leftover
/// Queued tasks from a previously-crashed run. On a ventilator **restart** mid-operation
/// (KNOWN_ISSUES D-4 band-aid), pass the live `progress_queue` ids so tasks a worker is *currently
/// processing* are NOT reset — resetting an in-flight task re-leases it while its original result
/// is still pending (a double-dispatch). With an empty slice this is exactly the old blunt reset
/// (`x <> ALL('{}')` is vacuously true), so process-start behaviour is unchanged.
pub(crate) fn clear_limbo_tasks_except(
  connection: &mut PgConnection,
  in_flight: &[i64],
) -> Result<usize, Error> {
  use crate::schema::tasks::dsl::{id, status};
  update(tasks::table)
    .filter(status.gt(&TaskStatus::TODO.raw()))
    .filter(id.ne_all(in_flight))
    .set(status.eq(&TaskStatus::TODO.raw()))
    .execute(connection)
}
