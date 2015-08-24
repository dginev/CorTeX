// Copyright 2015 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate zmq;
extern crate rand;

use zmq::{Error, Message, Context, SNDMORE};
use std::ops::Deref;
use std::thread;
use rand::{random};

pub trait Worker {
  fn work(&self, &Message) -> Option<Message>;
  fn service(&self) -> String;
  fn source(&self) -> String;
  fn sink(&self) -> String;

  fn start(&self, limit : Option<i32>) -> Result<(), Error> {
    let mut work_counter = 0;
    // Connect to a task ventilator
    let mut context_source = Context::new();
    let mut source = context_source.socket(zmq::DEALER).unwrap();
    let identity : String = (0..10).map(|_| rand::random::<u8>() as char).collect();
    source.set_identity(identity.as_bytes()).unwrap();

    assert!(source.connect(&self.source()).is_ok());
    // Connect to a task sink
    let mut context_sink = Context::new();
    let mut sink = context_sink.socket(zmq::PUSH).unwrap();
    assert!(sink.connect(&self.sink()).is_ok());
    // Work in perpetuity
    loop {
      let mut taskid_msg = Message::new().unwrap();
      let mut recv_msg = Message::new().unwrap();

      source.send_str(&self.service(), 0).unwrap();
      source.recv(&mut taskid_msg, 0).unwrap();
      let taskid = taskid_msg.as_str().unwrap();
      
      source.recv(&mut recv_msg, 0).unwrap();
      match self.work(&recv_msg) {
        Some(payload) => {
          sink.send_str(taskid, SNDMORE).unwrap();
          sink.send_msg(payload, 0).unwrap();
        },
        None => {
          // If there was nothing to do, retry a minute later
          thread::sleep_ms(60000);
          continue 
        }
      };

      work_counter += 1;
      match limit {
        Some(upper_bound) => {
          if work_counter >= upper_bound {
            // Give enough time to complete last job.
            thread::sleep_ms(500);
            break;
          }
        },
        None => {}
      };
    }
    Ok(())
  }
}
pub struct EchoWorker {
  pub service : String,
  pub version : f32,
  pub source : String,
  pub sink : String
}
impl Default for EchoWorker {
  fn default() -> EchoWorker {
    EchoWorker {
      service: "echo_service".to_string(),
      version: 0.1,
      source: "tcp://localhost:5555".to_string(),
      sink: "tcp://localhost:5556".to_string()      
    }
  }
}
impl Worker for EchoWorker {
  fn service(&self) -> String {self.service.clone()}
  fn source(&self) -> String {self.source.clone()}
  fn sink(&self) -> String {self.sink.clone()}

  fn work(&self, message : &Message) -> Option<Message> {
    let payload = message.deref();
    if payload.is_empty() {
      None }
    else {
      Some(Message::from_slice(payload).unwrap())
    }
  }
}