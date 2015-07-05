extern crate cortex;
extern crate rustlibxml;
extern crate zmq;

// A dispatcher executable for CorTeX distributed processing with ZMQ
// Binds REP socket to tcp://*:5555

use std::collections::HashMap;
use std::thread;

fn main() {
    let mut context = zmq::Context::new();
    let mut responder = context.socket(zmq::REP).unwrap();

    assert!(responder.bind("tcp://*:5555").is_ok());

    let mut msg = zmq::Message::new().unwrap();
    loop {
        responder.recv(&mut msg, 0).unwrap();
        println!("Received {}", msg.as_str().unwrap());
        responder.send_str("World", 0).unwrap();        
    }
}