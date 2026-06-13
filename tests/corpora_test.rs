// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! High-level contract test for the corpus-management capability (read side).

use cortex::backend::{self, build_pool, test_db_address};
use cortex::frontend::server::mount_api_with;
use cortex::models::{Corpus, NewCorpus, NewService, NewTask, Service};
use cortex::schema::{corpora, services, tasks};
use diesel::prelude::*;
use rocket::http::{ContentType, Status};
use rocket::local::blocking::Client;

fn client() -> Client {
  let pool = build_pool(test_db_address(), 4);
  let config_file = std::env::temp_dir().join("cortex_corpora_test.toml");
  Client::tracked(mount_api_with(rocket::build(), config_file, pool))
    .expect("a valid rocket instance")
}

#[test]
fn api_corpora_lists_registered_corpora() {
  let name = "corpora_capability_test";
  let mut db = backend::testdb();
  let _ = diesel::delete(corpora::table.filter(corpora::name.eq(name))).execute(&mut db.connection);
  db.add(&NewCorpus {
    name: name.to_string(),
    path: "/tmp/corpora_capability_test".to_string(),
    complex: true,
    description: "a test corpus".to_string(),
  })
  .expect("insert a corpus");

  let client = client();
  let response = client.get("/api/corpora").dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::JSON));

  let body: serde_json::Value = response.into_json().expect("a JSON body");
  let list = body.as_array().expect("a JSON array");
  let ours = list
    .iter()
    .find(|c| c["name"] == name)
    .expect("our corpus is listed");
  // Data contract:
  assert!(ours["path"].is_string());
  assert!(ours["description"].is_string());
  assert!(ours["complex"].is_boolean());

  let _ = diesel::delete(corpora::table.filter(corpora::name.eq(name))).execute(&mut db.connection);
}

fn cleanup(db: &mut backend::Backend, corpus_name: &str, service_name: &str) {
  if let Ok(corpus) = Corpus::find_by_name(corpus_name, &mut db.connection) {
    let _ = diesel::delete(tasks::table.filter(tasks::corpus_id.eq(corpus.id)))
      .execute(&mut db.connection);
    let _ =
      diesel::delete(corpora::table.filter(corpora::id.eq(corpus.id))).execute(&mut db.connection);
  }
  let _ = diesel::delete(services::table.filter(services::name.eq(service_name)))
    .execute(&mut db.connection);
}

#[test]
fn api_corpus_detail_reports_services_and_counts() {
  let corpus_name = "corpus_detail_test";
  let service_name = "corpus_detail_svc";
  let mut db = backend::testdb();
  cleanup(&mut db, corpus_name, service_name);

  db.add(&NewCorpus {
    name: corpus_name.to_string(),
    path: "/tmp/corpus_detail_test".to_string(),
    complex: true,
    description: "d".to_string(),
  })
  .expect("insert corpus");
  let corpus = Corpus::find_by_name(corpus_name, &mut db.connection).unwrap();
  db.add(&NewService {
    name: service_name.to_string(),
    version: 0.1,
    inputformat: "tex".to_string(),
    outputformat: "html".to_string(),
    inputconverter: None,
    complex: true,
    description: "d".to_string(),
  })
  .expect("insert service");
  let service = Service::find_by_name(service_name, &mut db.connection).unwrap();
  db.add(&NewTask {
    service_id: service.id,
    corpus_id: corpus.id,
    status: -1, // no_problem
    entry: "/tmp/corpus_detail_test/1/1.zip".to_string(),
  })
  .expect("insert task");

  let client = client();
  let path = format!("/api/corpora/{corpus_name}");
  let response = client.get(path.as_str()).dispatch();
  assert_eq!(response.status(), Status::Ok);

  let body: serde_json::Value = response.into_json().expect("a JSON body");
  assert_eq!(body["name"], corpus_name);
  let services_arr = body["services"].as_array().expect("a services array");
  let svc = services_arr
    .iter()
    .find(|s| s["name"] == service_name)
    .expect("the activated service is listed");
  assert_eq!(svc["total"], 1);
  assert_eq!(svc["no_problem"], 1);

  cleanup(&mut db, corpus_name, service_name);
}

#[test]
fn api_corpus_detail_is_404_for_unknown_corpus() {
  let client = client();
  let response = client.get("/api/corpora/no_such_corpus_xyz").dispatch();
  assert_eq!(response.status(), Status::NotFound);
}
