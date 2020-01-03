use crate::models::User;
use crate::schema::users;
use diesel::pg::PgConnection;
use diesel::*;

pub fn list_users(connection: &PgConnection) -> Vec<User> {
  users::table.load(connection).unwrap_or_default()
}
