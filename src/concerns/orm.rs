use diesel::pg::PgConnection;
use diesel::result::Error;

/// A minimalistic ORM trait for `CorTeX` data items
pub trait CortexInsertable {
  /// Creates a new item given a connection
  fn create(&self, connection: &mut PgConnection) -> Result<usize, Error>;
}

/// A minimalistic ORM trait for `CorTeX` data items
pub trait CortexDeletable {
  /// Creates a new item given a connection
  fn delete_by(&self, connection: &mut PgConnection, field: &str) -> Result<usize, Error>;
}
