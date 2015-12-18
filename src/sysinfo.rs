// Copyright 2015 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
use sys_info::*;
use std::collections::HashMap;


pub fn report(store : &mut HashMap<String,String>) -> Result<(),Error> {
  
  store.insert("sys_os_type".to_string(),os_type().unwrap());
  store.insert("sys_os_release".to_string(), os_release().unwrap());
  store.insert("sys_cpu".to_string(), cpu_num().unwrap().to_string());
  store.insert("sys_cpu_speed".to_string(),cpu_speed().unwrap().to_string());
  
  store.insert("sys_proc_total".to_string(),proc_total().unwrap().to_string());
  let load = loadavg().unwrap();
  store.insert("sys_load_one".to_string(), load.one.to_string());
  store.insert("sys_load_five".to_string(), load.five.to_string());
  store.insert("sys_load_fifteen".to_string(), load.fifteen.to_string());
  let mem = mem_info().unwrap();
  store.insert("sys_mem_total".to_string(),mem.total.to_string());
  store.insert("sys_mem_free".to_string(),mem.free.to_string());
  store.insert("sys_mem_avail".to_string(),mem.avail.to_string());
  store.insert("sys_mem_buffers".to_string(),mem.buffers.to_string());
  store.insert("sys_mem_cached".to_string(),mem.cached.to_string());

  store.insert("sys_mem_swap_total".to_string(),mem.swap_total.to_string());
  store.insert("sys_mem_swap_free".to_string(),mem.swap_free.to_string());
  
  let disk = disk_info().unwrap();
  store.insert("sys_disk_total".to_string(), disk.total.to_string());
  store.insert("sys_disk_free".to_string(), disk.free.to_string());
  store.insert("sys_hostname".to_string(),hostname().unwrap());
  Ok(())
}