// Copyright 2015 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

pub struct Dispatcher {
  pub port : usize,
  pub queue_size : usize,
}

impl Default for Dispatcher {
  fn default() -> Dispatcher {
    Dispatcher {
      port : 5555,
      queue_size : 100,
    }
  }
}
impl Dispatcher {
  pub fn start(&self) {

  }
}