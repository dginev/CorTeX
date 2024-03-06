//! Aggregate methods, to be used by backend
use crate::helpers::TaskStatus;
use crate::models::{Service, Task};
use crate::schema::tasks;
use diesel::result::Error;
use diesel::*;
use rand::{thread_rng, Rng};

pub(crate) fn fetch_tasks(
  connection: &PgConnection,
  service: &Service,
  queue_size: usize,
) -> Result<Vec<Task>, Error>
{
  use crate::schema::tasks::dsl::{service_id, status};
  let mut rng = thread_rng();
  let mark: u16 = 1 + rng.gen::<u16>();

  let mut marked_tasks: Vec<Task> = Vec::new();
  connection.transaction::<(), Error, _>(|| {
    let tasks_for_update = tasks::table
      .for_update()
      .filter(service_id.eq(service.id))
      .filter(status.eq(TaskStatus::TODO.raw()))
      .limit(queue_size as i64)
      .load(connection)?;
    marked_tasks = tasks_for_update
      .into_iter()
      .map(|task| Task {
        status: i32::from(mark),
        ..task
      })
      .map(|task| task.save_changes(connection))
      .filter_map(Result::ok)
      .collect();
    Ok(())
  })?;
  Ok(marked_tasks)
}

pub(crate) fn clear_limbo_tasks(connection: &PgConnection) -> Result<usize, Error> {
  use crate::schema::tasks::dsl::status;
  update(tasks::table)
    .filter(status.gt(&TaskStatus::TODO.raw()))
    .set(status.eq(&TaskStatus::TODO.raw()))
    .execute(connection)
}
