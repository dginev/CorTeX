use crate::concerns::*;
use crate::schema::daemons;
use diesel::result::Error;
use diesel::*;
use std::time::SystemTime;

#[derive(Identifiable, Queryable, Clone, Debug, PartialEq, Eq)]
#[table_name = "daemons"]
/// A daemon process record
pub struct DaemonProcess {
  /// primary key, auto-incremented by postgresql
  pub id: i32,
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
/// A new daemon process record
pub struct NewDaemonProcess {
  /// target for this permissions set
  pub pid: i32,
  /// timestamp on process spawn
  pub first_seen: SystemTime,
  /// timestamp on last lookup for pid freshness
  pub last_seen: SystemTime,
  /// cargo binary name
  pub name: String,
}

impl CortexInsertable for NewDaemonProcess {
  fn create(&self, connection: &PgConnection) -> Result<usize, Error> {
    insert_into(daemons::table).values(self).execute(connection)
  }
}

impl DaemonProcess {
  /// custom ORM-like for now, until diesel has a best practice
  pub fn find_by_name(name_query: &str, connection: &PgConnection) -> Result<Self, Error> {
    use daemons::dsl::name;
    daemons::table.filter(name.eq(name_query)).first(connection)
  }

  /// Deletes a daemon record from the DB (likely because it was reaped)
  pub fn destroy(self, connection: &PgConnection) -> Result<usize, Error> {
    delete(daemons::table)
      .filter(daemons::id.eq(self.id))
      .execute(connection)
  }
}
