use crate::schema::user_permissions;
use diesel::*;

#[derive(Identifiable, Queryable, Clone, Debug, PartialEq, Eq)]
#[table_name = "user_permissions"]
/// A `CorTeX` frontend user
pub struct UserPermission {
  /// primary key, auto-incremented by postgresql
  pub id: i64,
  /// target for this permissions set
  pub user_id: i32,
  /// permissions scoped to a given corpus - no scope means ALL
  pub corpus_id: Option<i32>,
  /// permissions scoped to a given service - no scope means ALL
  pub service_id: Option<i32>,
  /// owner of scope (full access)
  pub owner: bool,
  /// developer of scope (execute, rerun access)
  pub developer: bool,
  /// viewer of scope (read access)
  pub viewer: bool,
}

#[derive(Insertable, Debug, Clone)]
#[table_name = "user_permissions"]
/// A new task, to be inserted into `CorTeX`
pub struct NewUserPermission {
  /// target for this permissions set
  pub user_id: i32,
  /// permissions scoped to a given corpus - no scope means ALL
  pub corpus_id: Option<i32>,
  /// permissions scoped to a given service - no scope means ALL
  pub service_id: Option<i32>,
  /// owner of scope (full access)
  pub owner: bool,
  /// developer of scope (execute, rerun access)
  pub developer: bool,
  /// viewer of scope (read access)
  pub viewer: bool,
}
