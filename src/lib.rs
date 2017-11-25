// Copyright 2015-2016 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! A general purpose processing framework for corpora of scientific documents

#![doc(html_root_url = "https://dginev.github.io/rust-cortex/")]
#![doc(html_logo_url = "https://raw.githubusercontent.com/dginev/rust-cortex/master/public/img/logo.jpg")]
#![deny(missing_docs)]
#![recursion_limit="256"]
#![feature(plugin)]
#![plugin(dotenv_macros)]
extern crate glob;
extern crate libxml;
extern crate zmq;
extern crate libc;
extern crate regex;
extern crate postgres;
extern crate sys_info;
extern crate Archive;
extern crate rustc_serialize;
extern crate rand;
extern crate tempfile;
extern crate pericortex;
extern crate time;
#[macro_use] extern crate diesel;
#[macro_use] extern crate diesel_codegen;
extern crate dotenv;

pub mod schema;
pub mod concerns;
pub mod models;
pub mod helpers;
pub mod reports;
pub mod backend;

pub mod importer;
pub mod sysinfo;
pub mod manager;
pub mod worker;
