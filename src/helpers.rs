// Copyright 2015-2016 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Helper structures and methods for Task
use std::fmt;
use models::{Task, LogInvalid, LogInfo, LogWarning, LogError, LogFatal, LogRecord};

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
/// Enum for all types of reported messages for a given Task, as per the `LaTeXML` convention
/// One of "invalid", "fatal", "error", "warning" or "info"
pub enum TaskMessage {
  /// Debug/low-priroity messages
  Info(LogInfo),
  /// Soft/resumable problem messages
  Warning(LogWarning),
  /// Hard/recoverable problem messages
  Error(LogError),
  /// Critical/unrecoverable problem messages
  Fatal(LogFatal),
  /// Invalid tasks, work can not begin
  Invalid(LogInvalid),
}
impl LogRecord for TaskMessage {
  fn category(&self) -> &str {
    use helpers::TaskMessage::*;
    match *self {
      Info(ref record) => record.category(),
      Warning(ref record) => record.category(),
      Error(ref record) => record.category(),
      Fatal(ref record) => record.category(),
      Invalid(ref record) => record.category(),
    }
  }
  fn what(&self) -> &str {
    use helpers::TaskMessage::*;
    match *self {
      Info(ref record) => record.what(),
      Warning(ref record) => record.what(),
      Error(ref record) => record.what(),
      Fatal(ref record) => record.what(),
      Invalid(ref record) => record.what(),
    }
  }
  fn details(&self) -> &str {
    use helpers::TaskMessage::*;
    match *self {
      Info(ref record) => record.details(),
      Warning(ref record) => record.details(),
      Error(ref record) => record.details(),
      Fatal(ref record) => record.details(),
      Invalid(ref record) => record.details(),
    }
  }
  fn severity(&self) -> &str {
    use helpers::TaskMessage::*;
    match *self {
      Info(ref record) => record.severity(),
      Warning(ref record) => record.severity(),
      Error(ref record) => record.severity(),
      Fatal(ref record) => record.severity(),
      Invalid(ref record) => record.severity(),
    }
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
      TaskStatus::Blocked(x) |
      TaskStatus::Queued(x) => x,
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
    }.to_string()
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
    [
      "no_problem",
      "warning",
      "error",
      "fatal",
      "invalid",
      "todo",
      "blocked",
      "queued",
    ].iter()
      .map(|&x| x.to_string())
      .collect::<Vec<_>>()
  }
}