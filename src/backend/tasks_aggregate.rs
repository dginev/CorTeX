//! Aggregate methods, to be used by backend
use crate::helpers::TaskStatus;
use crate::models::{Service, Task};
use crate::schema::tasks;
use diesel::result::Error;
use diesel::*;
use rand::RngExt;

pub(crate) fn fetch_tasks(
  connection: &mut PgConnection,
  service: &Service,
  queue_size: usize,
) -> Result<Vec<Task>, Error> {
  use crate::schema::tasks::dsl::{id, service_id, status};
  let mut rng = rand::rng();
  // Temporary lease mark: a positive sentinel in the live-lease value space `[1, 65536]` — disjoint
  // from `TODO` (0), completed (`< 0`), and the rerun sentinel (`> 65536`, R-13). Computed in `i32`
  // so `1 + u16::MAX` reaches 65536 (the documented top of the lease range, see `helpers.rs`)
  // rather than overflowing `u16` back to 0/`TODO`.
  let mark: i32 = 1 + i32::from(rng.random::<u16>());

  connection.transaction::<Vec<Task>, Error, _>(|t_connection| {
    // Claim up to `queue_size` of this service's TODO tasks, locking the rows for the txn so a
    // concurrent (or future second) dispatcher can't grab the same ones; `SKIP LOCKED` makes such a
    // dispatcher take the next disjoint batch instead of blocking on ours.
    let claimed: Vec<i64> = tasks::table
      .filter(service_id.eq(service.id))
      .filter(status.eq(TaskStatus::TODO.raw()))
      .limit(queue_size as i64)
      .select(id)
      .for_update()
      .skip_locked()
      .load(t_connection)?;
    if claimed.is_empty() {
      return Ok(Vec::new());
    }
    // Mark the whole claimed batch Queued in ONE statement (was one `save_changes` UPDATE per row —
    // the D-8 write-amplification class, here on the lease side: ~`queue_size` round-trips per
    // refetch, serial on the single ventilator thread, each rewriting every column). `RETURNING *`
    // hands back the marked tasks the ventilator wraps into its dispatch queue.
    update(tasks::table.filter(id.eq_any(&claimed)))
      .set(status.eq(mark))
      .get_results::<Task>(t_connection)
  })
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
