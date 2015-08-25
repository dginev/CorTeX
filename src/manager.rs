// Copyright 2015 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate zmq;

use zmq::{Error, SNDMORE};
use backend::{Backend, DEFAULT_DB_ADDRESS};
use data::{Task, Service};

use std::thread;
use std::sync::Arc;
use std::sync::Mutex;

use std::ops::Deref;
use std::collections::HashMap;
// use std::fs::File;
// use std::io::prelude::*;

pub struct TaskManager {
  pub source_port : usize,
  pub result_port : usize,
  pub queue_size : usize,
  pub backend_address : String
}
pub struct Server {
  pub port : usize,
  pub queue_size : usize,
  pub backend : Backend
}

impl Default for TaskManager {
  fn default() -> TaskManager {
    TaskManager {
        source_port : 5555,
        result_port : 5555,
        queue_size : 100,
        backend_address : DEFAULT_DB_ADDRESS.clone().to_string() 
    } } }

impl TaskManager {
  pub fn start<'manager>(&'manager self) -> Result<(), Error> {
    // We'll use some local memoization shared between source and sink:
    let services: HashMap<String, Option<Service>> = HashMap::new();
    let progress_queue: HashMap<i64, Task> = HashMap::new();

    let services_arc = Arc::new(Mutex::new(services));
    let progress_queue_arc = Arc::new(Mutex::new(progress_queue));
    // First prepare the source ventilator
    let source_port = self.source_port.clone();
    let source_queue_size = self.queue_size.clone();
    let source_backend_address = self.backend_address.clone();

    let vent_services_arc = services_arc.clone();
    let vent_progress_queue_arc = progress_queue_arc.clone();
    let vent_thread = thread::spawn(move || {
      let sources = Server {
        port : source_port,
        queue_size : source_queue_size,
        backend : Backend::from_address(&source_backend_address)
      };
      sources.start_ventilator(vent_services_arc, vent_progress_queue_arc).unwrap();
    });

    // Now prepare the results sink
    let result_port = self.result_port.clone();
    let result_queue_size = self.queue_size.clone();
    let result_backend_address = self.backend_address.clone();

    let sink_services_arc = services_arc.clone();
    let sink_progress_queue_arc = progress_queue_arc.clone();
    let sink_thread = thread::spawn(move || {
      let results = Server {
        port : result_port,
        queue_size : result_queue_size,
        backend : Backend::from_address(&result_backend_address)
      };
      results.start_sink(sink_services_arc, sink_progress_queue_arc).unwrap();
    });

    vent_thread.join().unwrap();
    sink_thread.join().unwrap();
    Ok(())
  }
}

impl Server {
  pub fn start_ventilator(&self, 
      services_arc : Arc<Mutex<HashMap<String, Option<Service>>>>,
      progress_queue_arc : Arc<Mutex<HashMap<i64, Task>>>)
      -> Result <(),Error> {
    // We have a Ventilator-exclusive "queues" stack for tasks to be dispatched
    let mut queues : HashMap<String, Vec<Task>> = HashMap::new();
    // Assuming this is the only And tidy up the postgres tasks:
    self.backend.clear_limbo_tasks().unwrap();
    // Ok, let's bind to a port and start broadcasting
    let mut context = zmq::Context::new();
    let mut source = context.socket(zmq::ROUTER).unwrap();
    let port_str = self.port.to_string();
    let address = "tcp://*:".to_string() + &port_str;
    assert!(source.bind(&address).is_ok());

    loop {
      let mut msg = zmq::Message::new().unwrap();
      let mut identity = zmq::Message::new().unwrap();
      source.recv(&mut identity, 0).unwrap();
      source.recv(&mut msg, 0).unwrap();
      let service_name = msg.as_str().unwrap().to_string();
      println!("Task requested for service: {}", service_name.clone());
      

      let mut dispatched_task : Option<Task> = None;
      match self.get_sync_service_record(&services_arc, service_name.clone()) {
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
              let taskid = current_task.id.unwrap();

              match service.prepare_input(current_task.clone()) {
                Ok(payload) => {
                  dispatched_task = Some(current_task);

                  source.send_msg(identity, SNDMORE).unwrap();
                  source.send_str(&taskid.to_string(), SNDMORE).unwrap();
                  source.send(&payload, 0).unwrap();
                },
                Err(_) => {} // TODO: smart handling of failures
              }
            },
            None => {}
          };
        }
      };
      // Record that a task has been dispatched in the progress queue
      if dispatched_task.is_some() {
        Server::push_progress_task(&progress_queue_arc, dispatched_task.unwrap());
      }
    }
  }

  pub fn start_sink(&self,
      services_arc : Arc<Mutex<HashMap<String, Option<Service>>>>,
      progress_queue_arc : Arc<Mutex<HashMap<i64, Task>>>)
      -> Result <(),Error> {
    println!("Starting up Sink");
    // Ok, let's bind to a port and start broadcasting
    let mut context = zmq::Context::new();
    let mut receiver = context.socket(zmq::PULL).unwrap();
    let port_str = self.port.to_string();
    let address = "tcp://*:".to_string() + &port_str;
    assert!(receiver.bind(&address).is_ok());

    let mut sink_count = 0;
    loop {
      let mut msg = zmq::Message::new().unwrap();
      let mut taskid_msg = zmq::Message::new().unwrap();
      let mut service_msg = zmq::Message::new().unwrap();

      receiver.recv(&mut service_msg, 0).unwrap();
      let service_name = service_msg.as_str().unwrap();
      
      receiver.recv(&mut taskid_msg, 0).unwrap();
      let taskid_str = taskid_msg.as_str().unwrap();
      let taskid = taskid_str.parse::<i64>().unwrap();

      receiver.recv(&mut msg, 0).unwrap();
      let payload = msg.deref();
      if payload.is_empty() {continue}
      sink_count += 1;
      println!("Sink job {}, message size: {}", sink_count, payload.len());

      match Server::pop_progress_task(&progress_queue_arc, taskid) {
        None => {} // TODO: No such task, what to do?
        Some(task) => {

          println!("{:?}", task);
          let service_option = Server::get_service_record(&services_arc, service_name.to_string());
          match service_option.clone() {
            None => {}, // TODO: Handle errors
            Some(service) => {
              println!("Service: {:?}", service);
              if service.id.unwrap() == task.serviceid {
                println!("Task and Service match up.");
              }    
            }
          };
        }
      }

      // let mut file = File::create("/tmp/cortex_sink_".to_string() + &sink_count.to_string()).unwrap();
      // file.write_all(&payload).unwrap();
    }
  }

  fn get_sync_service_record(&self, services_arc : &Arc<Mutex<HashMap<String, Option<Service>>>>, service_name : String) -> Option<Service> {
    let mut services = services_arc.lock().unwrap();
    let service_record = services.entry(service_name.clone()).or_insert(
      Service::from_name(&self.backend.connection, service_name.clone()).unwrap()).clone();
    service_record
  }

  fn get_service_record(services_arc : &Arc<Mutex<HashMap<String, Option<Service>>>>, service_name : String) -> Option<Service> {
    let services = services_arc.lock().unwrap();
    let service_record = services.get(&service_name);
    match service_record {
      None => None, // TODO: Handle errors
      Some(service_option) => service_option.clone()
    }
  }

  fn pop_progress_task(progress_queue_arc : &Arc<Mutex<HashMap<i64, Task>>>, taskid: i64) -> Option<Task> {
    let mut progress_queue = progress_queue_arc.lock().unwrap();
    progress_queue.remove(&taskid)
  }

  fn push_progress_task(progress_queue_arc : &Arc<Mutex<HashMap<i64, Task>>>, progress_task: Task) {
    let mut progress_queue = progress_queue_arc.lock().unwrap();
    progress_queue.insert(progress_task.id.unwrap(), progress_task);
  }
}