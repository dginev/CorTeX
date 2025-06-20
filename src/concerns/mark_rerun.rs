use diesel::result::Error;
use diesel::*;

use crate::helpers::TaskStatus;
use crate::models::{LogError, LogFatal, LogInfo, LogInvalid, LogWarning, Task};
use crate::schema::tasks;

/// Task reruns by a variety of selector granularity
pub trait MarkRerun {
  /// Most-specific rerun query, via both category and what filter
  fn mark_rerun_by_what(
    mark: i32,
    corpus_id: i32,
    service_id: i32,
    rerun_category: &str,
    rerun_what: &str,
    connection: &mut PgConnection,
  ) -> Result<usize, Error>;
  /// Mid-specificity `category`-filtered reruns
  fn mark_rerun_by_category(
    mark: i32,
    corpus_id: i32,
    service_id: i32,
    rerun_category: &str,
    connection: &mut PgConnection,
  ) -> Result<usize, Error>;
}

/// Info level reruns
impl MarkRerun for LogInfo {
  fn mark_rerun_by_what(
    mark: i32,
    corpus_id: i32,
    service_id: i32,
    rerun_category: &str,
    rerun_what: &str,
    connection: &mut PgConnection,
  ) -> Result<usize, Error> {
    use crate::schema::log_infos::dsl::{category, log_infos, task_id, what};
    let task_ids_to_rerun = log_infos
      .filter(category.eq(rerun_category))
      .filter(what.eq(rerun_what))
      .select(task_id)
      .distinct();

    update(tasks::table)
      .filter(tasks::corpus_id.eq(&corpus_id))
      .filter(tasks::service_id.eq(&service_id))
      .filter(tasks::id.eq_any(task_ids_to_rerun))
      .set(tasks::status.eq(mark))
      .execute(connection)
  }

  fn mark_rerun_by_category(
    mark: i32,
    corpus_id: i32,
    service_id: i32,
    rerun_category: &str,
    connection: &mut PgConnection,
  ) -> Result<usize, Error> {
    use crate::schema::log_infos::dsl::{category, log_infos, task_id};
    let task_ids_to_rerun = log_infos
      .filter(category.eq(rerun_category))
      .select(task_id)
      .distinct();

    update(tasks::table)
      .filter(tasks::corpus_id.eq(&corpus_id))
      .filter(tasks::service_id.eq(&service_id))
      .filter(tasks::id.eq_any(task_ids_to_rerun))
      .set(tasks::status.eq(mark))
      .execute(connection)
  }
}

/// Warning level reruns
impl MarkRerun for LogWarning {
  fn mark_rerun_by_what(
    mark: i32,
    corpus_id: i32,
    service_id: i32,
    rerun_category: &str,
    rerun_what: &str,
    connection: &mut PgConnection,
  ) -> Result<usize, Error> {
    use crate::schema::log_warnings::dsl::{category, log_warnings, task_id, what};
    let task_ids_to_rerun = log_warnings
      .filter(category.eq(rerun_category))
      .filter(what.eq(rerun_what))
      .select(task_id)
      .distinct();

    update(tasks::table)
      .filter(tasks::corpus_id.eq(&corpus_id))
      .filter(tasks::service_id.eq(&service_id))
      .filter(tasks::id.eq_any(task_ids_to_rerun))
      .set(tasks::status.eq(mark))
      .execute(connection)
  }

  fn mark_rerun_by_category(
    mark: i32,
    corpus_id: i32,
    service_id: i32,
    rerun_category: &str,
    connection: &mut PgConnection,
  ) -> Result<usize, Error> {
    use crate::schema::log_warnings::dsl::{category, log_warnings, task_id};
    let task_ids_to_rerun = log_warnings
      .filter(category.eq(rerun_category))
      .select(task_id)
      .distinct();

    update(tasks::table)
      .filter(tasks::corpus_id.eq(&corpus_id))
      .filter(tasks::service_id.eq(&service_id))
      .filter(tasks::id.eq_any(task_ids_to_rerun))
      .set(tasks::status.eq(mark))
      .execute(connection)
  }
}

/// Error level reruns
impl MarkRerun for LogError {
  fn mark_rerun_by_what(
    mark: i32,
    corpus_id: i32,
    service_id: i32,
    rerun_category: &str,
    rerun_what: &str,
    connection: &mut PgConnection,
  ) -> Result<usize, Error> {
    use crate::schema::log_errors::dsl::{category, log_errors, task_id, what};
    let task_ids_to_rerun = log_errors
      .filter(category.eq(rerun_category))
      .filter(what.eq(rerun_what))
      .select(task_id)
      .distinct();

    update(tasks::table)
      .filter(tasks::corpus_id.eq(&corpus_id))
      .filter(tasks::service_id.eq(&service_id))
      .filter(tasks::id.eq_any(task_ids_to_rerun))
      .set(tasks::status.eq(mark))
      .execute(connection)
  }

  fn mark_rerun_by_category(
    mark: i32,
    corpus_id: i32,
    service_id: i32,
    rerun_category: &str,
    connection: &mut PgConnection,
  ) -> Result<usize, Error> {
    use crate::schema::log_errors::dsl::{category, log_errors, task_id};
    let task_ids_to_rerun = log_errors
      .filter(category.eq(rerun_category))
      .select(task_id)
      .distinct();

    update(tasks::table)
      .filter(tasks::corpus_id.eq(&corpus_id))
      .filter(tasks::service_id.eq(&service_id))
      .filter(tasks::id.eq_any(task_ids_to_rerun))
      .set(tasks::status.eq(mark))
      .execute(connection)
  }
}
/// Fatal level reruns
impl MarkRerun for LogFatal {
  fn mark_rerun_by_what(
    mark: i32,
    corpus_id: i32,
    service_id: i32,
    rerun_category: &str,
    rerun_what: &str,
    connection: &mut PgConnection,
  ) -> Result<usize, Error> {
    use crate::schema::log_fatals::dsl::{category, log_fatals, task_id, what};
    let task_ids_to_rerun = log_fatals
      .filter(category.eq(rerun_category))
      .filter(what.eq(rerun_what))
      .select(task_id)
      .distinct();

    update(tasks::table)
      .filter(tasks::corpus_id.eq(&corpus_id))
      .filter(tasks::service_id.eq(&service_id))
      .filter(tasks::id.eq_any(task_ids_to_rerun))
      .set(tasks::status.eq(mark))
      .execute(connection)
  }

  fn mark_rerun_by_category(
    mark: i32,
    corpus_id: i32,
    service_id: i32,
    rerun_category: &str,
    connection: &mut PgConnection,
  ) -> Result<usize, Error> {
    use crate::schema::log_fatals::dsl::{category, log_fatals, task_id};
    use diesel::sql_types::BigInt;
    if rerun_category == "no_messages" {
      let no_messages_query_string = "SELECT * FROM tasks t WHERE ".to_string()
        + "service_id=$1 and corpus_id=$2 and status=$3 and "
        + "NOT EXISTS (SELECT null FROM log_fatals where log_fatals.task_id=t.id)";
      let tasks_to_rerun: Vec<Task> = sql_query(no_messages_query_string)
        .bind::<BigInt, i64>(i64::from(service_id))
        .bind::<BigInt, i64>(i64::from(corpus_id))
        .bind::<BigInt, i64>(i64::from(TaskStatus::Fatal.raw()))
        .get_results(connection)
        .unwrap_or_default();
      let task_ids_to_rerun: Vec<i64> = tasks_to_rerun.iter().map(|t| t.id).collect();
      update(tasks::table)
        .filter(tasks::corpus_id.eq(&corpus_id))
        .filter(tasks::service_id.eq(&service_id))
        .filter(tasks::id.eq_any(task_ids_to_rerun))
        .set(tasks::status.eq(mark))
        .execute(connection)
    } else {
      let task_ids_to_rerun = log_fatals
        .filter(category.eq(rerun_category))
        .select(task_id)
        .distinct();

      update(tasks::table)
        .filter(tasks::corpus_id.eq(&corpus_id))
        .filter(tasks::service_id.eq(&service_id))
        .filter(tasks::id.eq_any(task_ids_to_rerun))
        .set(tasks::status.eq(mark))
        .execute(connection)
    }
  }
}

/// Invalid level reruns
impl MarkRerun for LogInvalid {
  fn mark_rerun_by_what(
    mark: i32,
    corpus_id: i32,
    service_id: i32,
    rerun_category: &str,
    rerun_what: &str,
    connection: &mut PgConnection,
  ) -> Result<usize, Error> {
    use crate::schema::log_invalids::dsl::{category, log_invalids, task_id, what};
    let task_ids_to_rerun = log_invalids
      .filter(category.eq(rerun_category))
      .filter(what.eq(rerun_what))
      .select(task_id)
      .distinct();

    update(tasks::table)
      .filter(tasks::corpus_id.eq(&corpus_id))
      .filter(tasks::service_id.eq(&service_id))
      .filter(tasks::id.eq_any(task_ids_to_rerun))
      .set(tasks::status.eq(mark))
      .execute(connection)
  }

  fn mark_rerun_by_category(
    mark: i32,
    corpus_id: i32,
    service_id: i32,
    rerun_category: &str,
    connection: &mut PgConnection,
  ) -> Result<usize, Error> {
    use crate::schema::log_invalids::dsl::{category, log_invalids, task_id};
    let task_ids_to_rerun = log_invalids
      .filter(category.eq(rerun_category))
      .select(task_id)
      .distinct();

    update(tasks::table)
      .filter(tasks::corpus_id.eq(&corpus_id))
      .filter(tasks::service_id.eq(&service_id))
      .filter(tasks::id.eq_any(task_ids_to_rerun))
      .set(tasks::status.eq(mark))
      .execute(connection)
  }
}
