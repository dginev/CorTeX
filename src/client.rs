// Copyright 2015 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate zmq;

use zmq::Error;
use backend::{Backend};
use data::{Task, Service};

use std::collections::HashMap;

pub struct Ventilator {
  pub port : usize,
  pub queue_size : usize,
  pub backend : Backend
}
pub struct Sink {
  pub port : usize,
  pub queue_size : usize,
  pub backend : Backend
}

impl Default for Ventilator {
  fn default() -> Ventilator {
    Ventilator {
      port : 5555,
      queue_size : 100,
      backend : Backend::default()
    } } }
impl Default for Sink {
  fn default() -> Sink {
    Sink {
      port : 5556,
      queue_size : 100,
      backend : Backend::default()
    } } }

impl Ventilator {
  pub fn start(&self) -> Result <(),Error>{
    // We'll use some local memoization:
    let mut services: HashMap<String, Option<Service>> = HashMap::new();

    // Ok, let's bind to a port and start broadcasting
    let mut context = zmq::Context::new();
    let mut source = context.socket(zmq::REP).unwrap();
    let port_str = self.port.to_string();
    let address = "tcp://*:".to_string() + &port_str;
    assert!(source.bind(&address).is_ok());

    let mut msg = zmq::Message::new().unwrap();
    let mut request_id = 0;
    loop {
      source.recv(&mut msg, 0).unwrap();
      let service_name = msg.as_str().unwrap().to_string();
      println!("Task requested for service: {}", service_name);
      
      let service_record = services.entry(service_name.clone()).or_insert(
        Service::from_name(&self.backend.connection, service_name).unwrap()).clone();

      match service_record {
        None => {},
        Some(service) => {
          let task_queue : Vec<Task> = self.backend.fetch_tasks(service, self.queue_size).unwrap();
          request_id += 1;
          source.send_str(&request_id.to_string(), 0).unwrap();
        }
      }
    }
  }
}

impl Sink {
  pub fn start(&self) -> Result <(),Error>{
    println!("Starting up Sink");
    // Ok, let's bind to a port and start broadcasting
    let mut context = zmq::Context::new();
    let mut receiver = context.socket(zmq::PULL).unwrap();
    let port_str = self.port.to_string();
    let address = "tcp://*:".to_string() + &port_str;
    assert!(receiver.bind(&address).is_ok());

    let mut msg = zmq::Message::new().unwrap();
    // Wait for start of batch
    println!("receiver ready to receive.");
    receiver.recv(&mut msg, 0).unwrap();
    println!("receiver init: {}", msg.as_str().unwrap());
    // We got contacted, let's receive for real:
    loop {
      receiver.recv(&mut msg, 0).unwrap();
      println!("Sink contacted: {}", msg.as_str().unwrap());
    }
  }
}