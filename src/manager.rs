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
use data::{TaskReport, TaskStatus, TaskProgress, TaskMessage, Service};

use std::thread;
use std::time::Duration;
use std::sync::Arc;
use std::sync::Mutex;

use std::ops::Deref;
use std::collections::HashMap;

use std::path::Path;
use std::fs::File;
// use tempfile::TempFile;
use std::io::{Write};
use std::io::Read;

use time;

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
  pub backend : Backend,
  pub backend_address : String
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
  pub fn start<'manager>(&'manager self, job_limit: Option<usize>) -> Result<(), Error> {
    // We'll use some local memoization shared between source and sink:
    let services: HashMap<String, Option<Service>> = HashMap::new();
    let progress_queue: HashMap<i64, TaskProgress> = HashMap::new();
    let done_queue: Vec<TaskReport> = Vec::new();

    let services_arc = Arc::new(Mutex::new(services));
    let progress_queue_arc = Arc::new(Mutex::new(progress_queue));
    let done_queue_arc = Arc::new(Mutex::new(done_queue));

    // First prepare the source ventilator
    let source_port = self.source_port.clone();
    let source_queue_size = self.queue_size.clone();
    let source_message_size = self.message_size.clone();
    let source_backend_address = self.backend_address.clone();

    let vent_services_arc = services_arc.clone();
    let vent_progress_queue_arc = progress_queue_arc.clone();
    let vent_done_queue_arc = done_queue_arc.clone();
    let vent_thread = thread::spawn(move || {
      let sources = Server {
        port : source_port,
        queue_size : source_queue_size,
        message_size : source_message_size,
        backend : Backend::from_address(&source_backend_address),
        backend_address : source_backend_address.clone()
      };
      sources.start_ventilator(vent_services_arc, vent_progress_queue_arc, vent_done_queue_arc, job_limit).unwrap();
    });

    // Next prepare the finalize thread which will persist finished jobs to the DB
    let finalize_backend_address = self.backend_address.clone();
    let finalize_done_queue_arc = done_queue_arc.clone();
    let finalize_thread = thread::spawn(move || {
      let finalize_backend = Backend::from_address(&finalize_backend_address);
      let mut finalize_jobs_count : usize = 0;
      // Persist every 1 second, if there is something to record
      'markdonejob: loop {
        match Server::mark_done_arc(&finalize_backend, &finalize_done_queue_arc) {
          true => {
            finalize_jobs_count+=1;
            true;
          } // we did some work, on to the next iteration
          false => { // If we have no reports to process, sleep for a second and recheck
            thread::sleep(Duration::new(1,0));
          }
        }
        if job_limit.is_some() && (finalize_jobs_count >= job_limit.unwrap()) {
          break
        }
      };
    });

    // Now prepare the results sink
    let result_port = self.result_port.clone();
    let result_queue_size = self.queue_size.clone();
    let result_message_size = self.message_size.clone();
    let result_backend_address = self.backend_address.clone();

    let sink_services_arc = services_arc.clone();
    let sink_progress_queue_arc = progress_queue_arc.clone();

    let sink_done_queue_arc = done_queue_arc.clone();
    let sink_thread = thread::spawn(move || {
      let results = Server {
        port : result_port,
        queue_size : result_queue_size,
        message_size : result_message_size,
        backend : Backend::from_address(&result_backend_address),
        backend_address: result_backend_address.clone()
      };
      results.start_sink(sink_services_arc, sink_progress_queue_arc, sink_done_queue_arc, job_limit).unwrap();
    });

    if vent_thread.join().is_err() {
      println!("Ventilator thread died unexpectedly!");
      Err(zmq::Error::ETERM)
    }
    else if sink_thread.join().is_err() {
      println!("Sink thread died unexpectedly!");
      Err(zmq::Error::ETERM)
    }
    else if finalize_thread.join().is_err() {
      println!("DB thread died unexpectedly!");
      Err(zmq::Error::ETERM)
    }
    else {
      println!("Manager successfully terminated!");
      Ok(())
    }
  }
}

impl Server {
  pub fn start_ventilator(&self, 
      services_arc : Arc<Mutex<HashMap<String, Option<Service>>>>,
      progress_queue_arc : Arc<Mutex<HashMap<i64, TaskProgress>>>,
      done_queue_arc : Arc<Mutex<Vec<TaskReport>>>,
      job_limit : Option<usize>)
      -> Result <(),Error> {
    // We have a Ventilator-exclusive "queues" stack for tasks to be dispatched
    let mut queues : HashMap<String, Vec<TaskProgress>> = HashMap::new();
    // Assuming this is the only And tidy up the postgres tasks:
    self.backend.clear_limbo_tasks().unwrap();
    // Ok, let's bind to a port and start broadcasting
    let mut context = zmq::Context::new();
    let mut ventilator = context.socket(zmq::ROUTER).unwrap();
    let port_str = self.port.to_string();
    let address = "tcp://*:".to_string() + &port_str;
    assert!(ventilator.bind(&address).is_ok());
    let mut source_job_count : usize = 0;

    'ventjob: loop {
      let mut msg = zmq::Message::new().unwrap();
      let mut identity = zmq::Message::new().unwrap();
      ventilator.recv(&mut identity, 0).unwrap();
      ventilator.recv(&mut msg, 0).unwrap();
      let service_name = msg.as_str().unwrap().to_string();
      // println!("Task requested for service: {}", service_name.clone());
      let request_time = time::get_time();
      source_job_count += 1;

      let mut dispatched_task : Option<TaskProgress> = None;
      match self.get_sync_service_record(&services_arc, service_name.clone()) {
        None => {},
        Some(service) => {
          if !queues.contains_key(&service_name) {
            queues.insert(service_name.clone(), Vec::new()); 
          }
          let mut task_queue : &mut Vec<TaskProgress> = queues.get_mut(&service_name).unwrap();
          if task_queue.is_empty() {
            // Refetch a new batch of tasks
            let now = time::get_time().sec;
            task_queue.extend(self.backend.fetch_tasks(&service, self.queue_size).unwrap()
              .into_iter().map(|task| TaskProgress {
                task: task,
                created_at : now,
                retries : 0
              })); 

            // This is a good time to also take care that none of the old tasks are dead in the progress queue
            // since the re-fetch happens infrequently, and directly implies the progress queue will grow
            let expired_tasks = Server::timeout_progress_tasks(&progress_queue_arc);
            for expired_t in expired_tasks {
              if expired_t.retries > 1 { // Too many retries, mark as fatal failure
                Server::push_done_queue(&done_queue_arc, TaskReport {
                  task : expired_t.task.clone(),
                  status : TaskStatus::Fatal,
                  messages :  vec![TaskMessage {
                    category : "cortex".to_string(),
                    severity : "fatal".to_string(), 
                    what : "never_completed_with_retries".to_string(), 
                    details : String::new()
                  }]
                });
              } else { // We can still retry, re-add to the dispatch queue
                task_queue.push(TaskProgress {
                  task : expired_t.task,
                  created_at : expired_t.created_at,
                  retries : expired_t.retries + 1
                });
              }
            }
          }
          match task_queue.pop() {
            Some(current_task_progress) => {
              dispatched_task = Some(current_task_progress.clone());

              let current_task = current_task_progress.task;
              let taskid = current_task.id.unwrap();
              let serviceid = current_task.serviceid;

              ventilator.send_msg(identity, SNDMORE).unwrap();
              ventilator.send_str(&taskid.to_string(), SNDMORE).unwrap();
              if serviceid == 1 { // No payload needed for init
                ventilator.send(&[],0).unwrap(); }
              else {
                // Regular services fetch the task payload and transfer it to the worker
                let file_opt = service.prepare_input_stream(current_task.clone());
                if file_opt.is_ok() {
                  let mut file = file_opt.unwrap();        
                  let mut total_outgoing : usize = 0;
                  'streaminputjob: loop {
                    // Stream input data via zmq
                    let mut data = vec![0; self.message_size];
                    let size = file.read(&mut data).unwrap();
                    total_outgoing += size;
                    data.truncate(size);
                    
                    if size < self.message_size {
                      // If exhausted, send the last frame
                      ventilator.send(&data,0).unwrap(); 
                      // And terminate
                      break
                    } else {
                      // If more to go, send the frame and indicate there's more to come
                      ventilator.send(&data,SNDMORE).unwrap();
                    }
                  }
                  let responded_time = time::get_time();
                  let request_duration = (responded_time - request_time).num_milliseconds();
                  println!("Source job {}, message size: {}, took {}ms.", source_job_count, total_outgoing, request_duration);
                } else {
                  // TODO: smart handling of failures
                  ventilator.send(&[],0).unwrap(); 
                }
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
      if job_limit.is_some() && (source_job_count >= job_limit.unwrap()) {
        break
      }
    }
    Ok(())
  }

  pub fn start_sink(&self,
      services_arc : Arc<Mutex<HashMap<String, Option<Service>>>>,
      progress_queue_arc : Arc<Mutex<HashMap<i64, TaskProgress>>>,
      done_queue_arc : Arc<Mutex<Vec<TaskReport>>>,
      job_limit: Option<usize>)
      -> Result <(),Error> {

    // Ok, let's bind to a port and start broadcasting
    let mut context = zmq::Context::new();
    let mut sink = context.socket(zmq::PULL).unwrap();
    let port_str = self.port.to_string();
    let address = "tcp://*:".to_string() + &port_str;
    assert!(sink.bind(&address).is_ok());

    let mut sink_job_count : usize = 0;

    'sinkjob: loop {
      let mut recv_msg = zmq::Message::new().unwrap();
      let mut taskid_msg = zmq::Message::new().unwrap();
      let mut service_msg = zmq::Message::new().unwrap();

      sink.recv(&mut service_msg, 0).unwrap();
      let service_name = match service_msg.as_str() {
        Some(some_name) => some_name,
        None => {"_unknown_"}
      };
      
      sink.recv(&mut taskid_msg, 0).unwrap();
      let taskid_str = match taskid_msg.as_str() {
        Some(some_id) => some_id,
        None => "-1"
      };
      let taskid = match taskid_str.parse::<i64>() {
        Ok(some_id) => some_id,
        Err(_) => -1
      };
      // We have a job, count it
      sink_job_count += 1;
      let mut total_incoming = 0;
      let request_time = time::get_time();
      println!("Incoming sink job {:?} for Service: {:?}, taskid: {:?}", sink_job_count, service_name, taskid_str);

      match Server::pop_progress_task(&progress_queue_arc, taskid) {
        None => {}, // TODO: No such task, what to do?
        Some(task_progress) => {
          let task = task_progress.task;
          let service_option = Server::get_service_record(&services_arc, service_name.to_string());
          match service_option.clone() {
            None => {
              println!("Error TODO: Server::get_service_record found nothing.");
            }, // TODO: Handle errors
            Some(service) => {
              let serviceid = match service.id {
                Some(found_id) => found_id,
                None => -1 // Skip if no such service 
              };
              // println!("Service: {:?}", serviceid);
              if serviceid == task.serviceid {
                // println!("Task and Service match up.");
                if serviceid == 1 { // No payload needed for init
                  match sink.recv(&mut recv_msg, 0) {
                    Ok(_) => {},
                    Err(e) => {
                      println!("Error TODO: sink.recv failed: {:?}",e);
                    }
                  };
                  let done_report = TaskReport {
                    task : task.clone(),
                    status : TaskStatus::NoProblem,
                    messages : Vec::new()
                  };
                  Server::push_done_queue(&done_queue_arc, done_report);
                }
                else {                
                  // Receive the rest of the input in the correct file
                  match Path::new(&task.entry).parent() {
                    None => {
                      println!("Error TODO: Path::new(&task.entry).parent() failed.");
                    },
                    Some(recv_dir) => {
                      match recv_dir.to_str() {
                        None => {
                          println!("Error TODO: recv_dir.to_str() failed");
                        },
                        Some(recv_dir_str) => {
                          let recv_dir_string = recv_dir_str.to_string();
                          let recv_pathname = recv_dir_string + "/" + &service.name + ".zip";
                          let recv_path = Path::new(&recv_pathname);
                          // println!("Will write to {:?}", recv_path);
                          { // Explicitly scope file, so that we drop it the moment we are done writing.
                            let mut file = match File::create(recv_path) {
                              Ok(f) => f,
                              Err(e) => {
                                println!("Error TODO: File::create(recv_path): {:?}", e);
                                continue;
                              }
                            };
                            'recvsinkjob: loop {
                              match sink.recv(&mut recv_msg, 0) {
                                Ok(_) => {},
                                Err(e) => {
                                  println!("Error TODO: sink.recv (line 309) failed: {:?}",e);
                                }
                              };

                              match file.write(recv_msg.deref()) {
                                Ok(written_bytes) => { total_incoming += written_bytes },
                                Err(e) => { 
                                  println!("Error TODO: file.write(recv_msg.deref()) failed: {:?}",e); 
                                  break;
                                }
                              };
                              match sink.get_rcvmore() {
                                Ok(false) => break,
                                Ok(true) => {},
                                Err(e) => {
                                  println!("Error TODO: sink.get_rcvmore failed: {:?}", e);
                                  break;
                                }
                              };
                            }
                            drop(file);
                          }
                          // Then mark the task done. This can be in a new thread later on
                          let done_report = task.generate_report(recv_path);
                          Server::push_done_queue(&done_queue_arc, done_report);
                        }
                      }
                    }
                  }
                }
                
              }
              else {
                // Otherwise just discard the rest of the message
                'discardjob: loop {
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
      let responded_time = time::get_time();
      let request_duration = (responded_time - request_time).num_milliseconds();
      println!("Sink job {}, message size: {}, took {}ms.", sink_job_count, total_incoming, request_duration);
      if job_limit.is_some() && (sink_job_count >= job_limit.unwrap()) {
        break
      }
    }
    Ok(())
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

  pub fn mark_done_arc(backend : &Backend, reports_arc: &Arc<Mutex<Vec<TaskReport>>>) -> bool {
    let reports = Server::fetch_shared_vec(reports_arc);
    if reports.len() > 0 {
      let request_time = time::get_time();
      backend.mark_done(&reports).unwrap(); // TODO: error handling if DB fails
      let responded_time = time::get_time();
      let request_duration = (responded_time - request_time).num_milliseconds();
      println!("Reporting done tasks to DB took {}ms.", request_duration);
      true
    } else {
      false
    }
  }
  pub fn push_done_queue(reports_arc : &Arc<Mutex<Vec<TaskReport>>>, report : TaskReport) {
    let mut reports = reports_arc.lock().unwrap();
    if reports.len() > 10000 {
      panic!("Done queue is too large: {:?} tasks. Stop the sink!", reports.len());
    }
    reports.push(report)
  }

  fn timeout_progress_tasks(progress_queue_arc : &Arc<Mutex<HashMap<i64, TaskProgress>>>) -> Vec<TaskProgress> {
    let mut progress_queue = progress_queue_arc.lock().unwrap();
    let now = time::get_time().sec;
    let expired_keys = progress_queue.iter()
                        .filter(|&(_, v)| v.expected_at() < now )
                        .map(|(k, _)| k.clone()).collect::<Vec<_>>();
    let mut expired_tasks = Vec::new();
    for key in expired_keys {
      match progress_queue.remove(&key) {
        None => {},
        Some(task_progress) => expired_tasks.push(task_progress)
      }
    }
    return expired_tasks
  }
  fn pop_progress_task(progress_queue_arc : &Arc<Mutex<HashMap<i64, TaskProgress>>>, taskid: i64) -> Option<TaskProgress> {
    let mut progress_queue = progress_queue_arc.lock().unwrap();
    progress_queue.remove(&taskid)
  }
  fn push_progress_task(progress_queue_arc : &Arc<Mutex<HashMap<i64, TaskProgress>>>, progress_task: TaskProgress) {
    let mut progress_queue = progress_queue_arc.lock().unwrap();
    // NOTE: This constant should be adjusted if you expect a fringe of more than 10,000 jobs 
    //       I am using this as a workaround for the inability to catch thread panic!() calls.
    if progress_queue.len() > 10000 { 
      panic!("Progress queue is too large: {:?} tasks. Stop the ventilator!",progress_queue.len());
    }
    match progress_task.task.id.clone() {
      Some(id) => {
        progress_queue.insert(id, progress_task);
      },
      None => {}
    };
  }

  fn fetch_shared_vec<T: Clone>(vec_arc: &Arc<Mutex<Vec<T>>>) -> Vec<T> {
    let mut vec_mutex_guard = vec_arc.lock().unwrap();
    let fetched_vec : Vec<T> = (*vec_mutex_guard).clone();
    vec_mutex_guard.clear();
    fetched_vec
  }
}