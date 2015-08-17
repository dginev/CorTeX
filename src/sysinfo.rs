// Copyright 2015 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
use sys_info::*;
use std::collections::HashMap;


pub fn report(store : &mut HashMap<&'static str,String>) -> Result<(),Error> {
  
  store.insert("sys_os_type",os_type().unwrap());
  store.insert("sys_os_release", os_release().unwrap());
  store.insert("sys_cpu", cpu_num().unwrap().to_string());
  store.insert("sys_cpu_speed",cpu_speed().unwrap().to_string());
  
  store.insert("sys_proc_total",proc_total().unwrap().to_string());
  let load = loadavg().unwrap();
  store.insert("sys_load_one", load.one.to_string());
  store.insert("sys_load_five", load.five.to_string());
  store.insert("sys_load_fifteen", load.fifteen.to_string());
  let mem = mem_info().unwrap();
  store.insert("sys_mem_total",mem.total.to_string());
  store.insert("sys_mem_free",mem.free.to_string());
  store.insert("sys_mem_avail",mem.avail.to_string());
  store.insert("sys_mem_buffers",mem.buffers.to_string());
  store.insert("sys_mem_cached",mem.cached.to_string());

  store.insert("sys_mem_swap_total",mem.swap_total.to_string());
  store.insert("sys_mem_swap_free",mem.swap_free.to_string());
  
  let disk = disk_info().unwrap();
  store.insert("sys_disk_total", disk.total.to_string());
  store.insert("sys_disk_free", disk.free.to_string());
  store.insert("sys_hostname",hostname().unwrap());
  Ok(())
}