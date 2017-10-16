// Copyright 2015-2016 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Backend models concerns and traits

use diesel::result::Error;
use diesel::pg::PgConnection;

/// A minimalistic ORM trait for `CorTeX` data items
pub trait CortexInsertable {
  /// Creates a new item given a connection
  fn create(&self, connection: &PgConnection) -> Result<usize, Error>;
}