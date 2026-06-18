// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Helper structures and methods for Task
use rand::{Rng, thread_rng};
use regex::Regex;
use std::fs::File;
use std::io;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::str;
use std::sync::LazyLock;

use diesel::pg::PgConnection;
use diesel::result::Error;

use crate::concerns::CortexInsertable;
use crate::models::{
  LogError, LogFatal, LogInfo, LogInvalid, LogRecord, LogWarning, NewLogError, NewLogFatal,
  NewLogInfo, NewLogInvalid, NewLogWarning, Task,
};

static MESSAGE_LINE_REGEX: LazyLock<Regex> =
  LazyLock::new(|| Regex::new(r"^([^ :]+):([^ :]+):([^ ]+)(\s(.*))?$").unwrap());
/// "(Loading... file" message regex
pub static LOADING_LINE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
  Regex::new(r"^\((?:Loading|Processing definitions)\s(.+/)?([^/]+[^.])\.\.\.(\s|$)").unwrap()
});
/// The short document name within an entry path: everything between the last `/` and the last `.`.
static ENTRY_DOCUMENT_NAME_REGEX: LazyLock<Regex> =
  LazyLock::new(|| Regex::new(r"^.+/(.+)\..+$").unwrap());

/// The short document name shown in reports and used for download filenames — an entry path's
/// basename without its directory or extension (e.g. `/data/…/0811.0417/0811.0417.zip` →
/// `0811.0417`). Falls back to the trimmed entry when the path doesn't match the `…/name.ext`
/// shape.
pub fn entry_document_name(entry: &str) -> String {
  let trimmed = entry.trim_end();
  ENTRY_DOCUMENT_NAME_REGEX.replace(trimmed, "$1").to_string()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
/// An enumeration of the expected task statuses. A small value type (every variant is a unit or a
/// single `i32`) that round-trips through `i32` via [`TaskStatus::raw`]/[`TaskStatus::from_raw`],
/// so it is `Copy`.
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

#[derive(Clone, Debug)]
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
  /// What is the latest admissible time for this task to be completed? The base deadline is the
  /// configured lease / visibility timeout (`dispatcher.lease_timeout_seconds`, default 1 h); each
  /// retry extends it by another full timeout (`(retries + 1) × timeout`), so a task that keeps
  /// timing out backs off rather than re-leasing ever-faster.
  pub fn expected_at(&self) -> i64 {
    self.created_at
      + ((self.retries + 1) * crate::config::config().dispatcher.lease_timeout_seconds)
  }
}

#[derive(Clone, Debug)]
/// Completed task, with its processing status and report messages
pub struct TaskReport {
  /// the `Task` we are reporting on
  pub task: Task,
  /// the reported processing status
  pub status: TaskStatus,
  /// a vector of `TaskMessage` log entries
  pub messages: Vec<NewTaskMessage>,
}

#[derive(Clone, Debug)]
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
    use crate::helpers::TaskMessage::*;
    match *self {
      Info(ref record) => record.task_id(),
      Warning(ref record) => record.task_id(),
      Error(ref record) => record.task_id(),
      Fatal(ref record) => record.task_id(),
      Invalid(ref record) => record.task_id(),
    }
  }
  fn category(&self) -> &str {
    use crate::helpers::TaskMessage::*;
    match *self {
      Info(ref record) => record.category(),
      Warning(ref record) => record.category(),
      Error(ref record) => record.category(),
      Fatal(ref record) => record.category(),
      Invalid(ref record) => record.category(),
    }
  }
  fn what(&self) -> &str {
    use crate::helpers::TaskMessage::*;
    match *self {
      Info(ref record) => record.what(),
      Warning(ref record) => record.what(),
      Error(ref record) => record.what(),
      Fatal(ref record) => record.what(),
      Invalid(ref record) => record.what(),
    }
  }
  fn details(&self) -> &str {
    use crate::helpers::TaskMessage::*;
    match *self {
      Info(ref record) => record.details(),
      Warning(ref record) => record.details(),
      Error(ref record) => record.details(),
      Fatal(ref record) => record.details(),
      Invalid(ref record) => record.details(),
    }
  }
  fn set_details(&mut self, new_details: String) {
    use crate::helpers::TaskMessage::*;
    match *self {
      Info(ref mut record) => record.set_details(new_details),
      Warning(ref mut record) => record.set_details(new_details),
      Error(ref mut record) => record.set_details(new_details),
      Fatal(ref mut record) => record.set_details(new_details),
      Invalid(ref mut record) => record.set_details(new_details),
    }
  }
  fn severity(&self) -> &str {
    use crate::helpers::TaskMessage::*;
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
  /// Maps the enumeration into the Postgresql table name expected to hold messages for this
  /// status
  pub fn to_table(&self) -> String {
    match *self {
      TaskStatus::Warning => "log_warnings",
      TaskStatus::Error => "log_errors",
      TaskStatus::Fatal => "log_fatals",
      TaskStatus::Invalid => "log_invalids",
      _ => "log_infos",
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
  pub fn from_key(key: &str) -> Option<Self> {
    match key.to_lowercase().as_str() {
      "no_problem" => Some(TaskStatus::NoProblem),
      "warning" => Some(TaskStatus::Warning),
      "error" => Some(TaskStatus::Error),
      "todo" => Some(TaskStatus::TODO),
      "in_progress" => Some(TaskStatus::TODO),
      "invalid" => Some(TaskStatus::Invalid),
      "blocked" => Some(TaskStatus::Blocked(-6)),
      "queued" => Some(TaskStatus::Queued(1)),
      "fatal" => Some(TaskStatus::Fatal),
      _ => None,
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
    ]
    .iter()
    .map(|&x| x.to_string())
    .collect::<Vec<_>>()
  }
}

#[derive(Clone, Debug)]
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
    use crate::helpers::NewTaskMessage::*;
    match *self {
      Info(ref record) => record.task_id(),
      Warning(ref record) => record.task_id(),
      Error(ref record) => record.task_id(),
      Fatal(ref record) => record.task_id(),
      Invalid(ref record) => record.task_id(),
    }
  }
  fn category(&self) -> &str {
    use crate::helpers::NewTaskMessage::*;
    match *self {
      Info(ref record) => record.category(),
      Warning(ref record) => record.category(),
      Error(ref record) => record.category(),
      Fatal(ref record) => record.category(),
      Invalid(ref record) => record.category(),
    }
  }
  fn what(&self) -> &str {
    use crate::helpers::NewTaskMessage::*;
    match *self {
      Info(ref record) => record.what(),
      Warning(ref record) => record.what(),
      Error(ref record) => record.what(),
      Fatal(ref record) => record.what(),
      Invalid(ref record) => record.what(),
    }
  }
  fn details(&self) -> &str {
    use crate::helpers::NewTaskMessage::*;
    match *self {
      Info(ref record) => record.details(),
      Warning(ref record) => record.details(),
      Error(ref record) => record.details(),
      Fatal(ref record) => record.details(),
      Invalid(ref record) => record.details(),
    }
  }
  fn set_details(&mut self, new_details: String) {
    use crate::helpers::NewTaskMessage::*;
    match *self {
      Info(ref mut record) => record.set_details(new_details),
      Warning(ref mut record) => record.set_details(new_details),
      Error(ref mut record) => record.set_details(new_details),
      Fatal(ref mut record) => record.set_details(new_details),
      Invalid(ref mut record) => record.set_details(new_details),
    }
  }

  fn severity(&self) -> &str {
    use crate::helpers::NewTaskMessage::*;
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
  fn create(&self, connection: &mut PgConnection) -> Result<usize, Error> {
    use crate::helpers::NewTaskMessage::*;
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
    severity: &str,
    category: String,
    what: String,
    details: String,
  ) -> NewTaskMessage {
    match severity.to_lowercase().as_str() {
      // Canonical Perl-LaTeXML token is "Warning"; tolerate the abbreviated "Warn" too so a
      // producer that emits `Warn:` doesn't silently land in the `_ => Info` default (which once
      // misfiled every latexml-oxide warning as Info).
      "warning" | "warn" => NewTaskMessage::Warning(NewLogWarning {
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
        category,
        task_id,
        what,
        details,
      }),
      "invalid" => NewTaskMessage::Invalid(NewLogInvalid {
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

/// Parses a log string which follows the `LaTeXML` convention
/// (described at [the Manual](http://dlmf.nist.gov/LaTeXML/manual/errorcodes/index.html))
pub fn parse_log(task_id: i64, log: &str) -> Vec<NewTaskMessage> {
  let mut messages: Vec<NewTaskMessage> = Vec::new();
  let mut in_details_mode = false;

  for line in log.lines() {
    // Skip empty lines
    if line.is_empty() {
      continue;
    }
    // If we have found a message header and we're collecting details:
    if in_details_mode {
      // If the line starts with tab, we are indeed reading in details
      if line.starts_with('\t') {
        // Append the details line to the last message. `in_details_mode` is only ever set true
        // right after a message is pushed, so `messages` is non-empty here by invariant — but we
        // never `.expect()`-panic the dispatch path on a hostile log (DESIGN_PRINCIPLES): if that
        // invariant is ever broken, treat the orphan details line as noise and carry on rather than
        // aborting the whole task's log parse.
        if let Some(mut last_message) = messages.pop() {
          let mut truncated_details = last_message.details().to_string() + "\n" + line;
          utf_truncate(&mut truncated_details, 2000);
          last_message.set_details(truncated_details);
          messages.push(last_message);
        }
        continue; // This line has been consumed, next
      } else {
        // Otherwise, no tab at the line beginning means last message has ended
        in_details_mode = false;
        if in_details_mode {} // hacky? disable "unused" warning
      }
    }
    // Since this isn't a details line, check if it's a message line:
    if let Some(cap) = MESSAGE_LINE_REGEX.captures(line) {
      // Indeed a message, so record it
      // We'll need to do some manual truncations, since the POSTGRESQL wrapper prefers
      //   panicking to auto-truncating (would not have been the Perl way, but Rust is Rust)
      let mut truncated_severity = cap
        .get(1)
        .map_or("", |m| m.as_str())
        .to_string()
        .to_lowercase();
      utf_truncate(&mut truncated_severity, 50);
      let mut truncated_category = cap.get(2).map_or("", |m| m.as_str()).to_string();
      utf_truncate(&mut truncated_category, 50);
      let mut truncated_what = cap.get(3).map_or("", |m| m.as_str()).to_string();
      utf_truncate(&mut truncated_what, 50);
      let mut truncated_details = cap.get(5).map_or("", |m| m.as_str()).to_string();
      utf_truncate(&mut truncated_details, 2000);

      if truncated_severity == "fatal" && truncated_category == "invalid" {
        truncated_severity = "invalid".to_string();
        truncated_category = truncated_what;
        truncated_what = "all".to_string();
      }

      let message = NewTaskMessage::new(
        task_id,
        &truncated_severity,
        truncated_category,
        truncated_what,
        truncated_details,
      );
      // Prepare to record follow-up lines with the message details:
      in_details_mode = true;
      // Add to the array of parsed messages
      messages.push(message);
    } else {
      in_details_mode = false; // not a details line.
      if let Some(cap) = LOADING_LINE_REGEX.captures(line) {
        // Special case is a "Loading..." info messages
        let mut filepath = cap.get(1).map_or("", |m| m.as_str()).to_string();
        let mut filename = cap.get(2).map_or("", |m| m.as_str()).to_string();
        utf_truncate(&mut filename, 50);
        filepath += &filename;
        utf_truncate(&mut filepath, 50);
        messages.push(NewTaskMessage::new(
          task_id,
          "info",
          "loaded_file".to_string(),
          filename,
          filepath,
        ));
      } else {
        // Otherwise line is just noise, continue...
      }
    }
  }
  messages
}

/// Decodes raw worker-log bytes into a string, **tolerating non-UTF-8 input** (arXiv data is
/// hostile and workers are unpredictable — W-2). A single invalid byte used to discard the whole
/// log and force-mark the task `Fatal`, throwing away every real conversion message + the true
/// status. Instead we decode lossily (invalid sequences → U+FFFD), preserving the real log, and
/// append a `Warning` line so the encoding issue is recorded *transparently* rather than silently
/// swallowed.
fn decode_worker_log(raw: &[u8]) -> String {
  match str::from_utf8(raw) {
    Ok(valid) => valid.to_string(),
    Err(_) => {
      let mut lossy = String::from_utf8_lossy(raw).into_owned();
      lossy.push_str(
        "\nWarning:cortex:non_utf8_log the worker log was not valid UTF-8; decoded lossily\n",
      );
      lossy
    },
  }
}

/// Reads + decodes the `cortex.log` entry out of a result `.zip`. Uses the pure-Rust `zip` crate's
/// **random-access `by_name`** — it seeks straight to `cortex.log` via the archive's central
/// directory, never decompressing the (potentially large) converted output (the per-task hot path;
/// ~1.4× libarchive on this op, see `docs/archive/ARCHIVE_RATIONALIZATION.md`). Returns the decoded
/// log text, or an `Err` describing why it couldn't (a non-zip / corrupt archive, or a missing
/// `cortex.log` → the task is left `Fatal`), rather than `.expect()`-panicking the dispatch path as
/// the old libarchive reader did.
fn read_cortex_log(result: &Path) -> Result<String, String> {
  let file = File::open(result).map_err(|e| format!("cannot open result archive: {e}"))?;
  let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("not a readable zip: {e}"))?;
  let mut entry = archive
    .by_name("cortex.log")
    .map_err(|e| format!("no cortex.log entry: {e}"))?;
  let mut raw = Vec::new();
  entry
    .read_to_end(&mut raw)
    .map_err(|e| format!("reading cortex.log failed: {e}"))?;
  Ok(decode_worker_log(&raw))
}

/// Generates a `TaskReport`, given the path to a result archive (`.zip`) from a `CorTeX` processing
/// job. Expects a `cortex.log` file in the archive, following the `LaTeXML` messaging conventions;
/// a missing/unreadable log leaves the task `Fatal` (the default).
pub fn generate_report(task: Task, result: &Path) -> TaskReport {
  let mut messages = Vec::new();
  let mut status = TaskStatus::Fatal; // Fatal by default
  match read_cortex_log(result) {
    Ok(log_string) => {
      // Look for the special status message - Fatal otherwise!
      for message in parse_log(task.id, &log_string).into_iter() {
        // Invalids are a bit of a workaround for now, they're fatal messages in latexml, but
        // we want them separated out in cortex
        let mut skip_message = false;
        match message {
          NewTaskMessage::Invalid(ref _log_invalid) => {
            status = TaskStatus::Invalid;
          },
          NewTaskMessage::Info(ref _log_info) => {
            let message_what = message.what();
            if message.category() == "conversion" && !message_what.is_empty() {
              // Adapt status to the CorTeX scheme: cortex_status = -(latexml_status+1)
              let latexml_scheme_status = match message_what.parse::<i32>() {
                Ok(num) => num,
                Err(e) => {
                  println!(
                    "-- generate_report: failed to parse conversion status {message_what:?}: {e:?}"
                  );
                  3 // latexml raw fatal
                },
              };
              let cortex_scheme_status = -(latexml_scheme_status + 1);
              if status != TaskStatus::Invalid {
                // Invalid status is final, and derived, all others are set directly from the log.
                status = TaskStatus::from_raw(cortex_scheme_status);
              }
              skip_message = true; // do not record the status message
            }
          },
          _ => {},
        };
        if !skip_message {
          messages.push(message);
        }
      }
    },
    Err(reason) => {
      println!("-- generate_report: {reason} (result {result:?}); task left Fatal");
    },
  }

  TaskReport {
    task,
    status,
    messages,
  }
}

/// Returns an open file handle to the task's entry
pub fn prepare_input_stream(task: &Task) -> Result<File, io::Error> {
  let entry_path = Path::new(&task.entry);
  File::open(entry_path)
}

/// The single source of truth for where a service's **result archive** lives, given a task's source
/// `entry`. The sink writes it here and the frontend reads it back from here — previously three
/// call sites re-derived the same `<entry-dir>/<service>.zip` three different ways (a
/// `Path::parent` and two subtly-different regexes), so this collapses them into one.
///
/// `sandbox_id` carries the F-6 fix: a **sandbox** corpus shares the parent's source `entry` paths
/// in place (owner decision: no source copy), so keying its outputs on `entry` alone would let a
/// sandbox rerun overwrite the parent's archives. When `Some(id)` (the sandbox's own corpus id) the
/// archive is name-scoped — `<entry-dir>/<service>.sandbox-<id>.zip` — so a sandbox's outputs never
/// collide with the parent's (or another sandbox's). `None` keeps the historical
/// `<entry-dir>/<service>.zip` for ordinary corpora (backward-compatible with existing archives).
///
/// Returns `None` if `entry` has no parent directory (a malformed/relative entry) — the caller then
/// has no result path to write or serve.
pub fn result_archive_path(
  entry: &str,
  service_name: &str,
  sandbox_id: Option<i32>,
) -> Option<PathBuf> {
  let dir = Path::new(entry.trim_end())
    .parent()
    .and_then(Path::to_str)?;
  let stem = match sandbox_id {
    Some(id) => format!("{service_name}.sandbox-{id}"),
    None => service_name.to_string(),
  };
  Some(PathBuf::from(format!("{dir}/{stem}.zip")))
}

/// Utility functions, until they find a better place
pub fn utf_truncate(input: &mut String, maxsize: usize) {
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
  *input = input.replace('\x00', "");
}

/// Generate a random integer useful for temporary DB marks
pub fn random_mark() -> i32 {
  let mut rng = thread_rng();
  let mark_rng: u16 = rng.r#gen();
  i32::from(mark_rng)
}

/// Helper for generating a random i32 in a range, to avoid loading the rng crate + boilerplate
pub fn rand_in_range(from: u16, to: u16) -> u16 {
  let mut rng = thread_rng();
  let mark_rng: u16 = rng.gen_range(from..=to);
  mark_rng
}

#[cfg(test)]
mod log_decode_tests {
  //! W-2: a non-UTF-8 worker log must degrade gracefully (decode lossily + record a warning), not
  //! get discarded wholesale with the task force-marked Fatal. DB-free, so no L-1 teardown risk.
  use super::{decode_worker_log, parse_log};
  use crate::models::LogRecord;

  #[test]
  fn valid_utf8_passes_through_unchanged() {
    let valid = "Warning:math:undefined hello\nStatus:conversion:1\n";
    assert_eq!(decode_worker_log(valid.as_bytes()), valid);
    assert!(
      !decode_worker_log(valid.as_bytes()).contains("non_utf8_log"),
      "no spurious warning is added to a clean log"
    );
  }

  #[test]
  fn non_utf8_decodes_lossily_not_fatal() {
    // A stray 0xFF byte in an otherwise-real conversion log.
    let raw = b"Warning:math:undefined bad \xFF byte\nStatus:conversion:1\n";
    let decoded = decode_worker_log(raw);
    // The real conversion status + the real message survive (the W-2 regression: previously the
    // whole log was thrown away and the task force-marked Fatal over this single byte).
    assert!(
      decoded.contains("Status:conversion:1"),
      "the real conversion status survives lossy decoding"
    );
    assert!(decoded.contains("Warning:math:undefined"));
    assert!(
      decoded.contains('\u{FFFD}'),
      "the invalid byte became the Unicode replacement char"
    );
    // The encoding issue is recorded transparently rather than silently swallowed.
    assert!(decoded.contains("Warning:cortex:non_utf8_log"));
    // And it parses into multiple real messages, not a single fatal.
    assert!(
      parse_log(1, &decoded).len() >= 2,
      "real messages are preserved, not collapsed into one fatal"
    );
  }

  #[test]
  fn warn_abbreviation_is_recognized_as_warning() {
    // latexml-oxide historically emitted the abbreviated `Warn:` token; cortex must file it as a
    // Warning, not silently default it to Info (the `_ => Info` arm), which once left every warning
    // task showing "no_messages" in the report. The canonical Perl token `Warning:` behaves the
    // same.
    use super::NewTaskMessage;
    let abbrev = parse_log(42, "Warn:missing_file:rotfloat.sty stubbed\n");
    assert_eq!(abbrev.len(), 1);
    assert!(
      matches!(abbrev[0], NewTaskMessage::Warning(_)),
      "abbreviated `Warn:` is filed as a Warning, not defaulted to Info"
    );
    let canonical = parse_log(42, "Warning:missing_file:x y\n");
    assert!(matches!(canonical[0], NewTaskMessage::Warning(_)));
  }

  #[test]
  fn read_cortex_log_extracts_from_zip_and_errors_gracefully() {
    use super::read_cortex_log;
    use std::fs::File;
    use std::io::Write;
    let zip_path = std::env::temp_dir().join("cortex_read_log_unit_test.zip");
    {
      let mut zw = zip::ZipWriter::new(File::create(&zip_path).unwrap());
      let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default();
      // A large output entry first; cortex.log last — `by_name` must seek straight past it.
      zw.start_file("html/index.html", opts).unwrap();
      zw.write_all(&vec![b'x'; 200_000]).unwrap();
      zw.start_file("cortex.log", opts).unwrap();
      zw.write_all(b"Info:cortex:hello a worker log line\n")
        .unwrap();
      zw.finish().unwrap();
    }
    let log = read_cortex_log(&zip_path).expect("reads cortex.log out of the zip");
    assert!(
      log.contains("hello a worker log line"),
      "by_name extracted the cortex.log content, got: {log:?}"
    );
    // A missing / non-zip path is a graceful Err (task left Fatal), never a panic.
    assert!(read_cortex_log(std::path::Path::new("/nonexistent/x.zip")).is_err());
    std::fs::remove_file(&zip_path).ok();
  }

  #[test]
  fn tab_indented_details_fold_into_the_preceding_message() {
    // A message header followed by tab-indented continuation lines: the details fold into the one
    // preceding message (the path that used to run through the `.expect()`).
    let log = "Error:math:bad first detail\n\tsecond line\n\tthird line\n";
    let messages = parse_log(7, log);
    assert_eq!(messages.len(), 1, "one message, the details folded into it");
    let details = messages[0].details();
    assert!(details.contains("first detail"), "got: {details:?}");
    assert!(details.contains("second line"), "got: {details:?}");
    assert!(details.contains("third line"), "got: {details:?}");
  }

  #[test]
  fn orphan_details_line_does_not_panic() {
    // A hostile log that opens with a tab-indented "details" line before any message header must be
    // treated as noise, never panic the dispatch path (DESIGN_PRINCIPLES: no `.expect()` on a
    // parse).
    let messages = parse_log(
      7,
      "\torphan detail with no header\nInfo:cortex:hi a real line\n",
    );
    assert_eq!(
      messages.len(),
      1,
      "the orphan line is ignored; the real message survives"
    );
  }

  #[test]
  fn result_archive_path_scopes_sandbox_outputs() {
    use super::result_archive_path;
    let entry = "/data/arxiv/1234/5678/source/5678.zip";
    // Ordinary corpus: the historical path, unchanged (backward-compatible).
    assert_eq!(
      result_archive_path(entry, "tex_to_html", None).unwrap(),
      std::path::PathBuf::from("/data/arxiv/1234/5678/source/tex_to_html.zip")
    );
    // Sandbox (F-6): same source dir, but a corpus-id-scoped archive name, so a sandbox rerun can
    // never overwrite the parent's `tex_to_html.zip`.
    assert_eq!(
      result_archive_path(entry, "tex_to_html", Some(42)).unwrap(),
      std::path::PathBuf::from("/data/arxiv/1234/5678/source/tex_to_html.sandbox-42.zip")
    );
    // A trailing newline (DB `text` columns carry them) is trimmed before taking the parent dir.
    assert_eq!(
      result_archive_path(&format!("{entry}\n"), "tex_to_html", None).unwrap(),
      std::path::PathBuf::from("/data/arxiv/1234/5678/source/tex_to_html.zip")
    );
    // No parent directory (a blank/root entry) ⇒ no result path: the caller skips the write/serve
    // instead of panicking.
    assert!(result_archive_path("", "tex_to_html", None).is_none());
    assert!(result_archive_path("/", "tex_to_html", None).is_none());
  }
}

#[cfg(test)]
mod entry_name_tests {
  use super::entry_document_name;

  #[test]
  fn extracts_short_document_name() {
    // The motivating UX example: a download should be named by the entry, not the task id.
    assert_eq!(
      entry_document_name("/data/arxmliv/0811/0811.0417/0811.0417.zip"),
      "0811.0417"
    );
    // Dotted arXiv ids keep their internal dot (the extension is the final segment).
    assert_eq!(
      entry_document_name("/data/foo/2105.13573.tar"),
      "2105.13573"
    );
    // A trailing-whitespace entry (padded varchar) is trimmed first.
    assert_eq!(entry_document_name("/d/bar.tex   "), "bar");
    // No `/name.ext` shape -> falls back to the trimmed entry rather than erroring.
    assert_eq!(entry_document_name("noslash"), "noslash");
  }
}
