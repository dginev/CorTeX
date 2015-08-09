// Copyright 2015 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

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
extern crate rustc_serialize;

pub mod backend;
pub mod importer;
pub mod data;
pub mod sysinfo;