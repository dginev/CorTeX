//! # The CorTeX library in Rust
//! The original library can be found at https://github.com/dginev/CorTeX

extern crate glob;
extern crate rustlibxml;
extern crate zmq;
extern crate libc;
extern crate regex;
extern crate postgres;
extern crate sys_info;
extern crate Archive;

pub mod backend;
pub mod importer;
pub mod data;
pub mod sysinfo;