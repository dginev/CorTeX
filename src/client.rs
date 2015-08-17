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

use std::ops::Deref;
use std::collections::HashMap;
// use std::fs::File;
// use std::io::prelude::*;

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
    let mut queues : HashMap<String, Vec<Task>> = HashMap::new();
    // Assuming this is the only And tidy up the postgres tasks:
    self.backend.clear_limbo_tasks().unwrap();
    // Ok, let's bind to a port and start broadcasting
    let mut context = zmq::Context::new();
    let mut source = context.socket(zmq::REP).unwrap();
    let port_str = self.port.to_string();
    let address = "tcp://*:".to_string() + &port_str;
    assert!(source.bind(&address).is_ok());

    let mut msg = zmq::Message::new().unwrap();
    loop {
      source.recv(&mut msg, 0).unwrap();
      let service_name = msg.as_str().unwrap().to_string();
      println!("Task requested for service: {}", service_name.clone());
      
      let service_record = services.entry(service_name.clone()).or_insert(
        Service::from_name(&self.backend.connection, service_name.clone()).unwrap()).clone();

      match service_record {
        None => {},
        Some(service) => {
          if !queues.contains_key(&service_name) {
            queues.insert(service_name.clone(), Vec::new()); 
          }
          let mut task_queue : &mut Vec<Task> = queues.get_mut(&service_name).unwrap();
          if task_queue.is_empty() {
            task_queue.extend(self.backend.fetch_tasks(&service, self.queue_size).unwrap()); }
          match task_queue.pop() {
            Some(current_task) => {
              println!("Preparing input for taskid : {:?}", current_task.id.unwrap());
              match service.prepare_input(current_task) {
                Ok(payload) => source.send(&payload, 0).unwrap(),
                Err(_) => source.send_str("", 0).unwrap() // TODO: smart handling of failures
              }
            },
            None => source.send_str("", 0).unwrap()
          };
        }
      };
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
    let mut sink_count = 0;
    // We got contacted, let's receive for real:
    loop {
      receiver.recv(&mut msg, 0).unwrap();
      let payload = msg.deref();
      if payload.is_empty() {continue}
      sink_count += 1;
      println!("Sink job {}, message size: {}", sink_count, payload.len());

      // let mut file = File::create("/tmp/cortex_sink_".to_string() + &sink_count.to_string()).unwrap();
      // file.write_all(&payload).unwrap();
    }
  }
}