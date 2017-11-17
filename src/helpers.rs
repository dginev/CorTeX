// Copyright 2015-2016 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Helper structures and methods for Task
use regex::Regex;
use models::{Task, LogInvalid, LogInfo, LogWarning, LogError, LogFatal, LogRecord, NewLogInvalid,
             NewLogInfo, NewLogWarning, NewLogError, NewLogFatal};
use diesel::pg::PgConnection;
use diesel::result::Error;
use concerns::CortexInsertable;

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
  pub messages: Vec<NewTaskMessage>,
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
  fn task_id(&self) -> i64 {
    use helpers::TaskMessage::*;
    match *self {
      Info(ref record) => record.task_id(),
      Warning(ref record) => record.task_id(),
      Error(ref record) => record.task_id(),
      Fatal(ref record) => record.task_id(),
      Invalid(ref record) => record.task_id(),
    }
  }
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
  fn set_details(&mut self, new_details: String) {
    use helpers::TaskMessage::*;
    match *self {
      Info(ref mut record) => record.set_details(new_details),
      Warning(ref mut record) => record.set_details(new_details),
      Error(ref mut record) => record.set_details(new_details),
      Fatal(ref mut record) => record.set_details(new_details),
      Invalid(ref mut record) => record.set_details(new_details),
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

#[derive(Clone)]
/// Enum for all types of reported messages for a given Task, as per the `LaTeXML` convention
/// One of "invalid", "fatal", "error", "warning" or "info"
pub enum NewTaskMessage {
  /// Debug/low-priroity messages
  Info(NewLogInfo),
  /// Soft/resumable problem messages
  Warning(NewLogWarning),
  /// Hard/recoverable problem messages
  Error(NewLogError),
  /// Critical/unrecoverable problem messages
  Fatal(NewLogFatal),
  /// Invalid tasks, work can not begin
  Invalid(NewLogInvalid),
}
impl LogRecord for NewTaskMessage {
  fn task_id(&self) -> i64 {
    use helpers::NewTaskMessage::*;
    match *self {
      Info(ref record) => record.task_id(),
      Warning(ref record) => record.task_id(),
      Error(ref record) => record.task_id(),
      Fatal(ref record) => record.task_id(),
      Invalid(ref record) => record.task_id(),
    }
  }
  fn category(&self) -> &str {
    use helpers::NewTaskMessage::*;
    match *self {
      Info(ref record) => record.category(),
      Warning(ref record) => record.category(),
      Error(ref record) => record.category(),
      Fatal(ref record) => record.category(),
      Invalid(ref record) => record.category(),
    }
  }
  fn what(&self) -> &str {
    use helpers::NewTaskMessage::*;
    match *self {
      Info(ref record) => record.what(),
      Warning(ref record) => record.what(),
      Error(ref record) => record.what(),
      Fatal(ref record) => record.what(),
      Invalid(ref record) => record.what(),
    }
  }
  fn details(&self) -> &str {
    use helpers::NewTaskMessage::*;
    match *self {
      Info(ref record) => record.details(),
      Warning(ref record) => record.details(),
      Error(ref record) => record.details(),
      Fatal(ref record) => record.details(),
      Invalid(ref record) => record.details(),
    }
  }
  fn set_details(&mut self, new_details: String) {
    use helpers::NewTaskMessage::*;
    match *self {
      Info(ref mut record) => record.set_details(new_details),
      Warning(ref mut record) => record.set_details(new_details),
      Error(ref mut record) => record.set_details(new_details),
      Fatal(ref mut record) => record.set_details(new_details),
      Invalid(ref mut record) => record.set_details(new_details),
    }
  }

  fn severity(&self) -> &str {
    use helpers::NewTaskMessage::*;
    match *self {
      Info(ref record) => record.severity(),
      Warning(ref record) => record.severity(),
      Error(ref record) => record.severity(),
      Fatal(ref record) => record.severity(),
      Invalid(ref record) => record.severity(),
    }
  }
}
impl CortexInsertable for NewTaskMessage {
  fn create(&self, connection: &PgConnection) -> Result<usize, Error> {
    use helpers::NewTaskMessage::*;
    match *self {
      Info(ref record) => record.create(connection),
      Warning(ref record) => record.create(connection),
      Error(ref record) => record.create(connection),
      Fatal(ref record) => record.create(connection),
      Invalid(ref record) => record.create(connection),
    }
  }
}

impl NewTaskMessage {
  /// Instantiates an appropriate insertable LogRecord object based on the raw message components
  pub fn new(
    task_id: i64,
    severity: String,
    category: String,
    what: String,
    details: String,
  ) -> NewTaskMessage {
    match severity.as_str() {
      "info" => NewTaskMessage::Info(NewLogInfo {
        task_id,
        category,
        what,
        details,
      }),
      "warning" => NewTaskMessage::Warning(NewLogWarning {
        task_id,
        category,
        what,
        details,
      }),
      "error" => NewTaskMessage::Error(NewLogError {
        task_id,
        category,
        what,
        details,
      }),
      "fatal" => NewTaskMessage::Fatal(NewLogFatal {
        task_id,
        category,
        what,
        details,
      }),
      _ => NewTaskMessage::Info(NewLogInfo {
        task_id,
        category,
        what,
        details,
      }), // unknown severity will be treated as info
    }
  }
}

/// Parses a log string which follows the LaTeXML convention
/// (described at http://dlmf.nist.gov/LaTeXML/manual/errorcodes/index.html)
pub fn parse_log(task_id: i64, log: String) -> Vec<NewTaskMessage> {
  let mut messages: Vec<NewTaskMessage> = Vec::new();
  let mut in_details_mode = false;

  // regexes:
  let message_line_regex = Regex::new(r"^([^ :]+):([^ :]+):([^ ]+)(\s(.*))?$").unwrap();
  for line in log.lines() {
    // Skip empty lines
    if line.is_empty() {
      continue;
    }
    // If we have found a message header and we're collecting details:
    if in_details_mode {
      // If the line starts with tab, we are indeed reading in details
      if line.starts_with('\t') {
        // Append details line to the last message
        let mut last_message = messages.pop().unwrap();
        let mut truncated_details = last_message.details().to_string() + "\n" + line;
        utf_truncate(&mut truncated_details, 2000);
        last_message.set_details(truncated_details);
        messages.push(last_message);
        continue; // This line has been consumed, next
      } else {
        // Otherwise, no tab at the line beginning means last message has ended
        in_details_mode = false;
        if in_details_mode {} // hacky? disable "unused" warning
      }
    }
    // Since this isn't a details line, check if it's a message line:
    match message_line_regex.captures(line) {
      Some(cap) => {
        // Indeed a message, so record it
        // We'll need to do some manual truncations, since the POSTGRESQL wrapper prefers
        //   panicking to auto-truncating (would not have been the Perl way, but Rust is Rust)
        let mut truncated_severity = cap.at(1).unwrap_or("").to_string().to_lowercase();
        utf_truncate(&mut truncated_severity, 50);
        let mut truncated_category = cap.at(2).unwrap_or("").to_string();
        utf_truncate(&mut truncated_category, 50);
        let mut truncated_what = cap.at(3).unwrap_or("").to_string();
        utf_truncate(&mut truncated_what, 50);
        let mut truncated_details = cap.at(5).unwrap_or("").to_string();
        utf_truncate(&mut truncated_details, 2000);

        if truncated_severity == "fatal" && truncated_category == "invalid" {
          truncated_severity = "invalid".to_string();
          truncated_category = truncated_what;
          truncated_what = "all".to_string();
        };

        let message = NewTaskMessage::new(
          task_id,
          truncated_severity,
          truncated_category,
          truncated_what,
          truncated_details,
        );
        // Prepare to record follow-up lines with the message details:
        in_details_mode = true;
        // Add to the array of parsed messages
        messages.push(message);
      }
      None => {
        // Otherwise line is just noise, continue...
        in_details_mode = false;
      }
    };
  }
  messages
}

/// Utility functions, until they find a better place
fn utf_truncate(input: &mut String, maxsize: usize) {
  let mut utf_maxsize = input.len();
  if utf_maxsize >= maxsize {
    {
      let mut char_iter = input.char_indices();
      while utf_maxsize >= maxsize {
        utf_maxsize = match char_iter.next_back() {
          Some((index, _)) => index,
          _ => 0,
        };
      }
    } // Extra {} wrap to limit the immutable borrow of char_indices()
    input.truncate(utf_maxsize);
  }
  // eliminate null characters if any
  *input = input.replace("\x00", "");
}
