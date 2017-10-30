// Copyright 2015-2016 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Helper structures and methods for Task 
use std::fmt;
use models::{Task};

#[derive(Clone, PartialEq, Eq)]
/// An enumeration of the expected task statuses
pub enum TaskStatus {
  /// currently queued for processing
  TODO,
  /// everything went smoothly
  NoProblem,
  /// minor issues
  Warning,
  /// major issues
  Error,
  /// critical/panic issues
  Fatal,
  /// invalid task, fatal + discard from statistics
  Invalid,
  /// currently blocked by dependencies
  Blocked(i32),
  /// currently being processed (marker identifies batch)
  Queued(i32),
}

#[derive(Clone)]
/// In-progress task, with dispatch metadata
pub struct TaskProgress {
  /// the `Task` struct being tracked
  pub task: Task,
  /// time of entering the job queue / first dispatch
  pub created_at: i64,
  /// number of dispatch retries
  pub retries: i64,
}
impl TaskProgress {
  /// What is the latest admissible time for this task to be completed?
  pub fn expected_at(&self) -> i64 {
    self.created_at + ((self.retries + 1) * 3600)
  }
}

#[derive(Clone)]
/// Completed task, with its processing status and report messages
pub struct TaskReport {
  /// the `Task` we are reporting on
  pub task: Task,
  /// the reported processing status
  pub status: TaskStatus,
  /// a vector of `TaskMessage` log entries
  pub messages: Vec<TaskMessage>,
}

#[derive(Clone)]
/// A task processing message, as per the `LaTeXML` convention
pub struct TaskMessage {
  /// high level description
  /// ("fatal", "error", "warning" or "info")
  pub severity: String,
  /// mid-level description (open set)
  pub category: String,
  /// low-level description (open set)
  pub what: String,
  /// technical details of the message (e.g. localization info)
  pub details: String,
}


impl fmt::Display for TaskMessage {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    write!(f,
           "(severity: {}, category: {},\n\twhat: {},\n\tdetails: {})\n",
           self.severity,
           self.category,
           self.what,
           self.details)
  }
}
impl fmt::Debug for TaskMessage {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    write!(f,
           "(severity: {}, category: {},\n\twhat: {},\n\tdetails: {})\n",
           self.severity,
           self.category,
           self.what,
           self.details)
  }
}
impl TaskStatus {
  /// Maps the enumeration into the raw ints for the Task store
  pub fn raw(&self) -> i32 {
    match *self {
      TaskStatus::TODO => 0,
      TaskStatus::NoProblem => -1,
      TaskStatus::Warning => -2,
      TaskStatus::Error => -3,
      TaskStatus::Fatal => -4,
      TaskStatus::Invalid => -5,
      TaskStatus::Blocked(x) | TaskStatus::Queued(x) => x,
    }
  }
  /// Maps the enumeration into the raw severity string for the Task store logs / frontend reports
  pub fn to_key(&self) -> String {
    match *self {
      TaskStatus::NoProblem => "no_problem",
      TaskStatus::Warning => "warning",
      TaskStatus::Error => "error",
      TaskStatus::Fatal => "fatal",
      TaskStatus::TODO => "todo",
      TaskStatus::Invalid => "invalid",
      TaskStatus::Blocked(_) => "blocked",
      TaskStatus::Queued(_) => "queued",
    }
    .to_string()
  }
  /// Maps from the raw Task store value into the enumeration
  pub fn from_raw(num: i32) -> Self {
    match num {
      0 => TaskStatus::TODO,
      -1 => TaskStatus::NoProblem,
      -2 => TaskStatus::Warning,
      -3 => TaskStatus::Error,
      -4 => TaskStatus::Fatal,
      -5 => TaskStatus::Invalid,
      num if num < -5 => TaskStatus::Blocked(num),
      _ => TaskStatus::Queued(num),
    }
  }
  /// Maps from the raw severity log values into the enumeration
  pub fn from_key(key: &str) -> Self {
    match key {
      "no_problem" => TaskStatus::NoProblem,
      "warning" => TaskStatus::Warning,
      "error" => TaskStatus::Error,
      "todo" => TaskStatus::TODO,
      "invalid" => TaskStatus::Invalid,
      "blocked" => TaskStatus::Blocked(-6),
      "queued" => TaskStatus::Queued(1),
      "fatal" | _ => TaskStatus::Fatal,
    }
  }
  /// Returns all raw severity strings as a vector
  pub fn keys() -> Vec<String> {
    ["no_problem", "warning", "error", "fatal", "invalid", "todo", "blocked", "queued"].iter().map(|&x| x.to_string()).collect::<Vec<_>>()
  }
}