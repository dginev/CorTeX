// Copyright 2015-2016 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Backend models and traits for the CorTeX "Task store"

use std::fmt;
use diesel::result::Error;
use diesel::{delete, insert_into};
use diesel::pg::PgConnection;
use diesel::prelude::*;
use schema::tasks;
use concerns::{CortexInsertable, CortexDeletable};

// Tasks

#[derive(Queryable,Clone)]
/// A CorTeX task, for a given corpus-service pair
pub struct Task {
  /// optional id (None for mock / yet-to-be-inserted rows)
  pub id: i64,
  /// id of the service owning this task
  pub serviceid: i32,
  /// id of the corpus hosting this task
  pub corpusid: i32,
  /// current processing status of this task
  pub status: i32,
  /// entry path on the file system
  pub entry: String
}

#[derive(Insertable)]
#[table_name="tasks"]
/// A new task, to be inserted into CorTeX
pub struct NewTask<'a> {
  /// id of the service owning this task
  pub serviceid: i32,
  /// id of the corpus hosting this task
  pub corpusid: i32,
  /// current processing status of this task
  pub status: i32,
  /// entry path on the file system
  pub entry: &'a str
}

impl fmt::Display for Task {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    // The `f` value implements the `Write` trait, which is what the
    // write! macro is expecting. Note that this formatting ignores the
    // various flags provided to format strings.
    write!(f,
           "(id: {}, entry: {},\n\tserviceid: {},\n\tcorpusid: {},\n\t status: {})\n",
           self.id,
           self.entry,
           self.serviceid,
           self.corpusid,
           self.status)
  }
}
impl fmt::Debug for Task {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    // The `f` value implements the `Write` trait, which is what the
    // write! macro is expecting. Note that this formatting ignores the
    // various flags provided to format strings.
    write!(f,
           "(id: {},\n\tentry: {},\n\tserviceid: {},\n\tcorpusid: {},\n\t status: {})\n",
           self.id,
           self.entry,
           self.serviceid,
           self.corpusid,
           self.status)
  }
}

impl<'a> CortexInsertable for NewTask<'a> {
  fn create(&self, connection: &PgConnection) -> Result<usize, Error> {
    insert_into(tasks::table).values(self).execute(connection)
  }
}

impl CortexDeletable for Task {
  fn delete_by(&self, connection: &PgConnection, field:&str) -> Result<usize, Error> {
    match field {
      "entry" => self.delete_by_entry(connection),
      "id" => self.delete_by_id(connection),
      _ => Err(Error::QueryBuilderError(format!("unknown Task model field: {}", field).into()))
    }
  }
}
impl Task {
  fn delete_by_entry(&self, connection: &PgConnection) -> Result<usize, Error> {
    use schema::tasks::dsl::entry;
    delete(tasks::table.filter(entry.eq(&self.entry))).execute(connection)
  }
  fn delete_by_id(&self, connection: &PgConnection) -> Result<usize, Error> {
    use schema::tasks::dsl::id;
    delete(tasks::table.filter(id.eq(self.id))).execute(connection) 
  }
}

impl<'a> CortexDeletable for NewTask<'a> {
  fn delete_by(&self, connection: &PgConnection, field:&str) -> Result<usize, Error> {
    match field {
      "entry" => self.delete_by_entry(connection),
      _ => Err(Error::QueryBuilderError(format!("unknown Task model field: {}", field).into()))
    }
  }
}

impl<'a> NewTask<'a> {
  fn delete_by_entry(&self, connection: &PgConnection) -> Result<usize, Error> {
    use schema::tasks::dsl::entry;
    delete(tasks::table.filter(entry.eq(&self.entry))).execute(connection)
  }
}