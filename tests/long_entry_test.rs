// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Regression test for KNOWN_ISSUES R-2: a document whose source-archive path exceeds the old
//! `varchar(200)` cap must be storable as a task (it used to error on insert and be lost).

use cortex::backend;
use cortex::models::NewTask;
use cortex::schema::tasks;
use diesel::prelude::*;

#[test]
fn long_entry_is_storable_without_truncation() {
  let mut backend = backend::testdb();
  // A source-archive path well past the old 200-char cap (a deep/hostile arXiv path).
  let long_entry = format!("/data/arxiv/{}/paper.zip", "x".repeat(300));
  assert!(long_entry.len() > 200, "exercises the widened column");

  let clean = |connection: &mut PgConnection| {
    diesel::delete(tasks::table.filter(tasks::entry.eq(&long_entry)))
      .execute(connection)
      .ok();
  };
  clean(&mut backend.connection);

  let inserted = backend.add(&NewTask {
    entry: long_entry.clone(),
    service_id: 1,
    corpus_id: 9_999_999,
    status: 0,
  });
  assert!(
    inserted.is_ok(),
    "a >200-char entry now stores: {inserted:?}"
  );

  // Read it back: the full path is preserved (varchar does not pad or truncate).
  let stored: String = tasks::table
    .filter(tasks::entry.eq(&long_entry))
    .select(tasks::entry)
    .first(&mut backend.connection)
    .expect("the long-entry task is retrievable");
  assert_eq!(stored, long_entry, "entry stored without truncation");

  clean(&mut backend.connection);
}
