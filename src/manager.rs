// Copyright 2015 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate zmq;
extern crate tempfile;

use zmq::{Error, SNDMORE};
use backend::{Backend, DEFAULT_DB_ADDRESS};
use data::{Task, Service};

use std::thread;
use std::sync::Arc;
use std::sync::Mutex;

use std::ops::Deref;
use std::collections::HashMap;

use std::path::Path;
use std::fs::File;
// use tempfile::TempFile;
use std::io::{Write};
use std::io::Read;

pub struct TaskManager {
  pub source_port : usize,
  pub result_port : usize,
  pub queue_size : usize,
  pub message_size : usize,
  pub backend_address : String
}
pub struct Server {
  pub port : usize,
  pub queue_size : usize,
  pub message_size : usize,
  pub backend : Backend
}

impl Default for TaskManager {
  fn default() -> TaskManager {
    TaskManager {
        source_port : 5555,
        result_port : 5555,
        queue_size : 100,
        message_size : 100000,
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
    let source_message_size = self.message_size.clone();
    let source_backend_address = self.backend_address.clone();

    let vent_services_arc = services_arc.clone();
    let vent_progress_queue_arc = progress_queue_arc.clone();
    let vent_thread = thread::spawn(move || {
      let sources = Server {
        port : source_port,
        queue_size : source_queue_size,
        message_size : source_message_size,
        backend : Backend::from_address(&source_backend_address)
      };
      sources.start_ventilator(vent_services_arc, vent_progress_queue_arc).unwrap();
    });

    // Now prepare the results sink
    let result_port = self.result_port.clone();
    let result_queue_size = self.queue_size.clone();
    let result_message_size = self.message_size.clone();
    let result_backend_address = self.backend_address.clone();

    let sink_services_arc = services_arc.clone();
    let sink_progress_queue_arc = progress_queue_arc.clone();
    let sink_thread = thread::spawn(move || {
      let results = Server {
        port : result_port,
        queue_size : result_queue_size,
        message_size : result_message_size,
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
    let mut ventilator = context.socket(zmq::ROUTER).unwrap();
    let port_str = self.port.to_string();
    let address = "tcp://*:".to_string() + &port_str;
    assert!(ventilator.bind(&address).is_ok());
    let mut source_job_count = 0;

    loop {
      let mut msg = zmq::Message::new().unwrap();
      let mut identity = zmq::Message::new().unwrap();
      ventilator.recv(&mut identity, 0).unwrap();
      ventilator.recv(&mut msg, 0).unwrap();
      let service_name = msg.as_str().unwrap().to_string();
      println!("Task requested for service: {}", service_name.clone());
      source_job_count += 1;

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
              let file_opt = service.prepare_input_stream(current_task.clone());
              if file_opt.is_ok() {
                let mut file = file_opt.unwrap();
                dispatched_task = Some(current_task);

                ventilator.send_msg(identity, SNDMORE).unwrap();
                ventilator.send_str(&taskid.to_string(), SNDMORE).unwrap();
                
                let mut total_outgoing = 0;
                loop {
                  // Stream input data via zmq
                  let mut data = vec![0; self.message_size];
                  let size = file.read(&mut data).unwrap();
                  total_outgoing += size;
                  data.truncate(size);
                  
                  if size < self.message_size {
                    // If exhausted, send the last frame
                    ventilator.send(&data,0).unwrap(); 
                    // And terminate
                    break;
                  } else {
                    // If more to go, send the frame and indicate there's more to come
                    ventilator.send(&data,SNDMORE).unwrap();
                  }
                }
                println!("Source job {}, message size: {}", source_job_count, total_outgoing);
              } else {
                // TODO: smart handling of failures
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

    // Ok, let's bind to a port and start broadcasting
    let mut context = zmq::Context::new();
    let mut sink = context.socket(zmq::PULL).unwrap();
    let port_str = self.port.to_string();
    let address = "tcp://*:".to_string() + &port_str;
    assert!(sink.bind(&address).is_ok());

    let mut sink_job_count = 0;
    let mut done_queue = Vec::new();
    loop {
      let mut recv_msg = zmq::Message::new().unwrap();
      let mut taskid_msg = zmq::Message::new().unwrap();
      let mut service_msg = zmq::Message::new().unwrap();

      sink.recv(&mut service_msg, 0).unwrap();
      let service_name = service_msg.as_str().unwrap();
      
      sink.recv(&mut taskid_msg, 0).unwrap();
      let taskid_str = taskid_msg.as_str().unwrap();
      let taskid = taskid_str.parse::<i64>().unwrap();
      // We have a job, count it
      sink_job_count += 1;
      let mut total_incoming = 0;

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
                
                // Receive the rest of the input in the correct file
                let recv_dir = Path::new(&task.entry).parent().unwrap();
                let recv_pathname = recv_dir.to_str().unwrap().to_string() + "/" + &service.name + "_" + &service.version.round().to_string() + ".zip";
                let recv_path = Path::new(&recv_pathname);
                println!("Will write to {:?}", recv_path);
                let mut file = File::create(recv_path).unwrap();
                loop {
                  sink.recv(&mut recv_msg, 0).unwrap();

                  total_incoming += file.write(recv_msg.deref()).unwrap();
                  if !sink.get_rcvmore().unwrap() {
                    break;
                  }
                }
                // Then mark the task done. This can be in a new thread later on
                let done_report = task.generate_report(recv_path);
                done_queue.push(done_report);
                if done_queue.len() >= 100 { // Persist every 100 reports.
                  self.backend.mark_done(&done_queue).unwrap(); // TODO: error handling if DB fails
                  done_queue.clear();
                }
              }
              else {
                // Otherwise just discard the rest of the message
                loop {
                  sink.recv(&mut recv_msg, 0).unwrap();
                  if !sink.get_rcvmore().unwrap() {
                    break;
                  }
                }
              }
            }
          };
        }
      }
      println!("Sink job {}, message size: {}", sink_job_count, total_incoming);

      // let mut file = File::create("/tmp/cortex_sink_".to_string() + &sink_job_count.to_string()).unwrap();
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