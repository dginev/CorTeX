use diesel::*;

use crate::schema::user_actions;
use std::time::SystemTime;

#[derive(Identifiable, Queryable, Clone, Debug, PartialEq, Eq, QueryableByName)]
#[table_name = "user_actions"]
/// A `CorTeX` frontend user
pub struct UserAction {
  /// primary key, auto-incremented by postgresql
  pub id: i64,
  /// action owner
  pub user_id: i32,
  /// corpus affected, if any
  pub corpus_id: Option<i32>,
  /// service affected, if any
  pub service_id: Option<i32>,
  /// counter for actions with potential quotas
  pub action_counter: i32,
  /// time of last action of this type
  pub last_timestamp: SystemTime,
  /// CorTeX location where action was initiated
  pub location: String,
  /// exact description of action
  pub description: String,
}

#[derive(Insertable, Debug, Clone)]
#[table_name = "user_actions"]
/// A new task, to be inserted into `CorTeX`
pub struct NewUserAction {
  /// action owner
  pub user_id: i32,
  /// corpus affected, if any
  pub corpus_id: Option<i32>,
  /// service affected, if any
  pub service_id: Option<i32>,
  /// counter for actions with potential quotas
  pub action_counter: i32,
  /// time of last action of this type
  pub last_timestamp: SystemTime,
  /// CorTeX location where action was initiated
  pub location: String,
  /// exact description of action
  pub description: String,
}
