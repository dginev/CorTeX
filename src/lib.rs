// Copyright 2015-2018 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! A general purpose processing framework for corpora of scientific documents

#![doc(html_root_url = "https://dginev.github.io/rust-cortex/")]
#![doc(
  html_logo_url = "https://raw.githubusercontent.com/dginev/rust-cortex/master/public/img/logo.jpg"
)]
#![deny(missing_docs)]
#![recursion_limit = "256"]
#![feature(plugin)]
#![allow(clippy::implicit_hasher)]
#[macro_use]
extern crate diesel;
#[macro_use]
extern crate dotenv_codegen;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate rocket;

pub mod backend;
pub mod concerns;
pub mod dispatcher;
pub mod frontend;
pub mod helpers;
pub mod importer;
pub mod models;
pub mod reports;
/// Auto-generated diesel schema for the backend DB
pub mod schema;
pub mod worker;
