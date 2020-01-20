use crate::concerns::*;
use crate::schema::daemons;
use diesel::result::Error;
use diesel::*;
use std::time::SystemTime;

/// A daemon process record
#[derive(Identifiable, Queryable, Clone, Debug, PartialEq, Eq)]
#[table_name = "daemons"]
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

/// A new daemon process record
#[derive(Insertable, Debug, Clone)]
#[table_name = "daemons"]
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

  /// Increment the `last_seen` field to the current time
  pub fn touch(&self, connection: &PgConnection) -> Result<usize, Error> {
    diesel::update(self)
      .set(daemons::last_seen.eq(SystemTime::now()))
      .execute(connection)
  }
}
