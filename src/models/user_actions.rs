use chrono::prelude::*;
use diesel::result::Error;
use diesel::*;
use serde::Serialize;
use std::time::SystemTime;

use crate::concerns::CortexInsertable;
use crate::models::{Corpus, Service, User};
use crate::schema::user_actions;

#[derive(Identifiable, Queryable, Clone, Debug, PartialEq, Eq, QueryableByName, Serialize)]
#[table_name = "user_actions"]
/// A `CorTeX` frontend user
pub struct UserAction {
  /// primary key, auto-incremented by postgresql
  pub id: i32,
  /// action owner
  pub user_id: i32,
  /// corpus affected, if any
  pub corpus_id: Option<i32>,
  /// service affected, if any
  pub service_id: Option<i32>,
  /// counter for actions with potential quotas
  pub action_counter: i32,
  /// time of last action of this type
  pub last_timestamp: NaiveDateTime,
  /// CorTeX location where action was initiated
  pub location: String,
  /// exact description of action
  pub description: String,
}

#[derive(Insertable, Debug, Clone)]
#[table_name = "user_actions"]
/// A new task, to be inserted into `CorTeX`
pub struct NewUserAction {
  /// action owner
  pub user_id: i32,
  /// corpus affected, if any
  pub corpus_id: Option<i32>,
  /// service affected, if any
  pub service_id: Option<i32>,
  /// counter for actions with potential quotas
  pub action_counter: i32,
  /// time of last action of this type
  pub last_timestamp: SystemTime,
  /// CorTeX location where action was initiated
  pub location: String,
  /// exact description of action
  pub description: String,
}

impl CortexInsertable for NewUserAction {
  fn create(&self, connection: &PgConnection) -> Result<usize, Error> {
    insert_into(user_actions::table)
      .values(self)
      .execute(connection)
  }
}

impl UserAction {
  /// custom ORM-like for now, until diesel has a best practice
  pub fn find_by_user_id(query: i32, connection: &PgConnection) -> Result<Self, Error> {
    use user_actions::dsl::user_id;
    user_actions::table
      .filter(user_id.eq(query))
      .first(connection)
  }

  /// custom ORM-like for now, until diesel has a best practice
  pub fn find_by_corpus_service(
    corpus_id: i32,
    service_id: i32,
    connection: &PgConnection,
  ) -> Result<Vec<UserAction>, Error>
  {
    use user_actions::dsl;
    user_actions::table
      .filter(dsl::corpus_id.eq(corpus_id))
      .filter(dsl::service_id.eq(service_id))
      .load(connection)
  }

  /// Return all known user actions, sorted by time recorded
  pub fn all(connection: &PgConnection) -> Result<Vec<UserAction>, Error> {
    user_actions::table.load(connection)
  }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
/// A human-readable user action for reports
pub struct UserActionReport {
  /// The name or email of the user performing the action
  pub user_name: String,
  /// Name of affected corpus, if any
  pub corpus_name: String,
  /// Name of affected service, if any
  pub service_name: String,
  /// Description of the action
  pub description: String,
  /// Exact location of the action (e.g. frontend URL, command-line script)
  pub location: String,
  /// Time when action was initiated
  pub timestamp: NaiveDateTime,
}

impl UserActionReport {
  /// Return all known user actions, stored by time recorded, in reportable form
  pub fn all(connection: &PgConnection) -> Result<Vec<UserActionReport>, Error> {
    let reports = UserAction::all(connection)?
      .into_iter()
      .map(|action| {
        let user = User::find(action.user_id, connection).unwrap();
        let user_name = if user.display.is_empty() {
          user.email
        } else {
          user.display
        };
        let mut corpus_name = String::new();
        if let Some(corpus_id) = action.corpus_id {
          let corpus = Corpus::find(corpus_id, connection).unwrap();
          corpus_name = if corpus.name.is_empty() {
            corpus.path
          } else {
            corpus.name
          };
        }
        let mut service_name = String::new();
        if let Some(service_id) = action.service_id {
          let service = Service::find(service_id, connection).unwrap();
          service_name = service.name;
        }

        UserActionReport {
          user_name,
          corpus_name,
          service_name,
          description: action.description,
          location: action.location,
          timestamp: action.last_timestamp,
        }
      })
      .collect();
    Ok(reports)
  }
}
