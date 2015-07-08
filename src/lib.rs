//! # The CorTeX library in Rust
//! The original library can be found at https://github.com/dginev/CorTeX

extern crate rustlibxml;
extern crate mysql;
extern crate zmq;
extern crate libc;
extern crate regex;
extern crate sys_info;

pub mod backend;
pub mod import;
pub mod data;
pub mod sysinfo;