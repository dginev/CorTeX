use crate::schema::daemons;
use diesel::*;
use std::time::SystemTime;

#[derive(Identifiable, Queryable, Clone, Debug, PartialEq, Eq)]
#[table_name = "daemons"]
/// A `CorTeX` frontend user
pub struct DaemonPid {
  /// primary key, auto-incremented by postgresql
  pub id: i64,
  /// target for this permissions set
  pub pid: i32,
  /// timestamp on process spawn
  pub first_seen: SystemTime,
  /// timestamp on last lookup for pid freshness
  pub last_seen: SystemTime,
  /// cargo binary name
  pub name: String,
}

#[derive(Insertable, Debug, Clone)]
#[table_name = "daemons"]
/// A new task, to be inserted into `CorTeX`
pub struct NewDaemonPid {
  /// target for this permissions set
  pub pid: i32,
  /// timestamp on process spawn
  pub first_seen: SystemTime,
  /// timestamp on last lookup for pid freshness
  pub last_seen: SystemTime,
  /// cargo binary name
  pub name: String,
}
