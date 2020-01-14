#![allow(clippy::implicit_hasher)]
use diesel::pg::PgConnection;
use diesel::result::Error;
use diesel::*;
use std::time::SystemTime;

use crate::concerns::{CortexDeletable, CortexInsertable};
use crate::schema::users;

// users

#[derive(Identifiable, Queryable, AsChangeset, Clone, Debug, PartialEq, Eq, QueryableByName)]
#[table_name = "users"]
/// A `CorTeX` frontend user
pub struct User {
  /// user primary key, auto-incremented by postgresql
  pub id: i32,
  /// display name for the user
  pub display: String,
  /// email with which the oauth service identifies this user
  pub email: String,
  /// user creation date
  pub first_seen: SystemTime,
  /// last registered activity with the backend
  pub last_seen: SystemTime,
  /// is the user an admin?
  pub admin: bool,
}

#[derive(Insertable, Debug, Clone)]
#[table_name = "users"]
/// A new task, to be inserted into `CorTeX`
pub struct NewUser {
  /// display name for the user
  pub display: String,
  /// email with which the oauth service identifies this user
  pub email: String,
  /// user creation date
  pub first_seen: SystemTime,
  /// last registered activity with the backend
  pub last_seen: SystemTime,
  /// is the user an admin?
  pub admin: bool,
}

impl CortexInsertable for NewUser {
  fn create(&self, connection: &PgConnection) -> Result<usize, Error> {
    insert_into(users::table).values(self).execute(connection)
  }
}

impl CortexDeletable for User {
  fn delete_by(&self, connection: &PgConnection, field: &str) -> Result<usize, Error> {
    match field {
      "email" => self.delete_by_email(connection),
      _ => Err(Error::QueryBuilderError(
        format!("unknown Task model field: {}", field).into(),
      )),
    }
  }
}

impl User {
  /// custom ORM-like for now, until diesel has a best practice
  pub fn find_by_email(email_query: &str, connection: &PgConnection) -> Result<Self, Error> {
    use users::dsl::email;
    users::table.filter(email.eq(email_query)).first(connection)
  }
  /// custom ORM-like for now, until diesel has a best practice
  pub fn delete_by_email(&self, connection: &PgConnection) -> Result<usize, Error> {
    use users::dsl::email;
    delete(users::table.filter(email.eq(&self.email))).execute(connection)
  }
}

impl NewUser {
  /// custom ORM-like for now, until diesel has a best practice
  pub fn delete_by_email(&self, connection: &PgConnection) -> Result<usize, Error> {
    use users::dsl::email;
    delete(users::table.filter(email.eq(&self.email))).execute(connection)
  }
  /// Creates the user unless already present in the DB (entry conflict)
  pub fn create_if_new(&self, connection: &PgConnection) -> Result<usize, Error> {
    insert_into(users::table)
      .values(self)
      .on_conflict_do_nothing()
      .execute(connection)
  }
}
