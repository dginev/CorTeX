// Copyright 2015 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

pub struct Receiver {
  pub port : usize,
  pub queue_size : usize,
}

impl Default for Receiver {
  fn default() -> Receiver {
    Receiver {
      port : 5555,
      queue_size : 100,
    }
  }
}
impl Receiver {
  pub fn start(&self) {
    
  }
}