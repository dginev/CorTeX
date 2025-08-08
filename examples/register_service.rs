// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Register a service with a given name on a corpus with a given path
//! Example run: `cargo run --release --example register_service tex_to_html /data/arxmliv/`
use cortex::backend::Backend;
use cortex::models::Service;
use std::env;

fn main() {
  let mut input_args = env::args();
  let _ = input_args.next();
  let service_name = input_args
    .next()
    .expect("Please provide service name as the first argument");
  let mut corpus_path = input_args
    .next()
    .expect("Please provide corpus path as the second argument");

  if let Some(c) = corpus_path.pop() {
    if c != '/' {
      corpus_path.push(c);
    }
  }
  corpus_path.push('/');

  println!(
    "-- Registering service {:?} on corpus at {:?} ...",
    &service_name, &corpus_path
  );
  let mut backend = Backend::default();
  let service_registered_result = Service::find_by_name(&service_name, &mut backend.connection);
  assert!(service_registered_result.is_ok());
  let service_registered = service_registered_result.unwrap();

  assert!(backend
    .register_service(&service_registered, &corpus_path)
    .is_ok());
}
