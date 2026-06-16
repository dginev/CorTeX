// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! `cortex` — the administration CLI. A thin renderer over the library: self-install (`init`),
//! diagnostics (`doctor`), DB tuning, token management, the `report`/`runs`/`document` read surface
//! (the CLI twins of the web/agent overview, run-history, and per-article forensics), and dataset
//! export.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use cortex::backend::{
  self, create_sandbox, default_db_address, export_html_dataset, task_messages,
  DatasetExportOutcome, GroupBy, RerunOptions, SandboxSelection,
};
use cortex::bootstrap::{self, DoctorReport};
use cortex::config::config_file_path;
use cortex::helpers::TaskStatus;
use cortex::models::{Corpus, HistoricalRun, Service, Task};

/// Formats a timestamp the same way the web/agent surfaces do (RFC 3339, seconds) so the CLI's run
/// JSON matches `RunDto`.
fn iso(time: chrono::NaiveDateTime) -> String {
  time
    .and_utc()
    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[derive(Parser)]
#[command(name = "cortex", version, about = "CorTeX administration CLI")]
struct Cli {
  #[command(subcommand)]
  command: Command,
}

#[derive(Subcommand)]
enum Command {
  /// Initialize the database (run embedded migrations) and scaffold a config file if missing.
  Init,
  /// Diagnose the installation (database reachable, migrations current, services seeded).
  Doctor {
    /// Emit the report as JSON instead of text.
    #[arg(long)]
    json: bool,
  },
  /// Print PostgreSQL server-tuning guidance for this host (pgtune inputs; see docs/DB_TUNING.md).
  TuneDb,
  /// Set or generate an admin/API token in cortex.toml's [auth] section (no hand-editing).
  SetAdminToken {
    /// The token value to set. Omit and pass --generate to create a random one.
    token: Option<String>,
    /// Generate a random token instead of supplying one (printed once).
    #[arg(long)]
    generate: bool,
    /// The owner this token is attributed to in the audit log (gives the actor an identity).
    #[arg(long, default_value = "admin")]
    owner: String,
  },
  /// Print the conversion-status overview for a `(corpus, service)` — the CLI twin of the
  /// web/agent service overview (`GET /api/reports/<c>/<s>`): the valid-task total + per-status
  /// counts/shares.
  Report {
    /// Corpus name.
    corpus: String,
    /// Service name (e.g. tex_to_html).
    service: String,
    /// Emit JSON (the same shape as the agent `ServiceOverviewDto`) instead of a text table.
    #[arg(long)]
    json: bool,
  },
  /// Run history for a `(corpus, service)` — the CLI twin of the web run-history screen + agent
  /// `GET /api/runs/<c>/<s>`: each conversion run with its per-severity tallies (live for the open
  /// run). The macro view of how conversion quality moved over time.
  Runs {
    /// Corpus name.
    corpus: String,
    /// Service name (e.g. tex_to_html).
    service: String,
    /// Emit JSON (the same shape as the agent `RunDto` list) instead of a text table.
    #[arg(long)]
    json: bool,
  },
  /// Per-article forensics for one document — the CLI twin of the web forensic screen + agent
  /// `GET /api/corpus/<c>/<svc>/document/<name>`: the document's status + every worker-log
  /// message.
  Document {
    /// Corpus name.
    corpus: String,
    /// Service name (e.g. tex_to_html).
    service: String,
    /// The document's short name as it appears in reports (e.g. 0801.1234).
    name: String,
    /// Include info-level messages (loaded files / debug noise), hidden by default.
    #[arg(long)]
    all: bool,
    /// Emit JSON (the same shape as the agent `DocumentReportDto`) instead of a text table.
    #[arg(long)]
    json: bool,
  },
  /// Mark a filtered scope of a `(corpus, service)` for reconversion — the CLI twin of the
  /// web/agent rerun. Resets the matching tasks to TODO so the dispatcher re-runs them.
  /// **Dry-run by default** (prints the scope); pass `--yes` to execute.
  Rerun {
    /// Corpus name.
    corpus: String,
    /// Service name (e.g. tex_to_html).
    service: String,
    /// Restrict to a severity (`no_problem`|`warning`|`error`|`fatal`|`invalid`). Omit = all.
    #[arg(long)]
    severity: Option<String>,
    /// Restrict to a message category (requires `--severity`).
    #[arg(long)]
    category: Option<String>,
    /// Restrict to a message `what` (requires `--severity --category`).
    #[arg(long)]
    what: Option<String>,
    /// Description recorded for the run (audit trail).
    #[arg(long)]
    description: Option<String>,
    /// Owner credited for the run (audit identity).
    #[arg(long, default_value = "admin")]
    owner: String,
    /// Actually execute the rerun (without this, the command is a dry run that only prints the
    /// scope).
    #[arg(long)]
    yes: bool,
  },
  /// Carve a sandbox corpus out of a parent by a message-condition filter — the CLI twin of the
  /// web/agent sandbox. A sandbox is a first-class corpus (its own tasks/runs/reports) you can
  /// then run/rerun to iterate a campaign on a subset. Dry-run by default; pass `--yes` to
  /// create.
  Sandbox {
    /// Parent corpus to carve from.
    parent: String,
    /// Name for the new sandbox corpus (must be unique).
    name: String,
    /// Service whose conversion results are filtered (e.g. tex_to_html).
    #[arg(long)]
    service: String,
    /// Severity to capture (`no_problem`|`warning`|`error`|`fatal`|`invalid`).
    #[arg(long)]
    severity: String,
    /// Restrict to a message category.
    #[arg(long)]
    category: Option<String>,
    /// Restrict to a message `what` within the category.
    #[arg(long)]
    what: Option<String>,
    /// Actually create the sandbox (without this, the command is a dry run that only prints the
    /// scope).
    #[arg(long)]
    yes: bool,
  },
  /// Delete a corpus (or sandbox) and all of its tasks + log messages — the CLI twin of the
  /// web/agent `DELETE /api/corpora/<name>`, via the transactional, orphan-free `Corpus::destroy`.
  /// Dry-run by default (prints the blast radius); pass `--yes` to delete. Historical run tallies
  /// are immutable and survive.
  DeleteCorpus {
    /// Corpus name to delete.
    name: String,
    /// Actually delete (without this, the command is a dry run that only prints the blast radius).
    #[arg(long)]
    yes: bool,
  },
  /// Bundle a corpus/service's converted HTML into ZIP datasets (replaces the
  /// bundle-html-dataset*.sh scripts). Reads existing result archives off the filesystem.
  ExportDataset {
    /// Corpus name to export.
    corpus: String,
    /// Service name whose HTML output is bundled (e.g. tex_to_html).
    service: String,
    /// Output directory for the archives + manifest (created if missing).
    #[arg(long)]
    out: PathBuf,
    /// Bucket archives by `month` (one zip per year-month) or `severity` (one zip per severity).
    #[arg(long, default_value = "month")]
    group_by: String,
    /// Comma-separated severities to include.
    #[arg(
      long,
      value_delimiter = ',',
      default_value = "no_problem,warning,error"
    )]
    severity: Vec<String>,
  },
}

fn main() {
  match Cli::parse().command {
    Command::Init => run_init(),
    Command::Doctor { json } => run_doctor(json),
    Command::Report {
      corpus,
      service,
      json,
    } => run_report(corpus, service, json),
    Command::Runs {
      corpus,
      service,
      json,
    } => run_runs(corpus, service, json),
    Command::Document {
      corpus,
      service,
      name,
      all,
      json,
    } => run_document(corpus, service, name, all, json),
    Command::Rerun {
      corpus,
      service,
      severity,
      category,
      what,
      description,
      owner,
      yes,
    } => run_rerun(
      corpus,
      service,
      severity,
      category,
      what,
      description,
      owner,
      yes,
    ),
    Command::Sandbox {
      parent,
      name,
      service,
      severity,
      category,
      what,
      yes,
    } => run_sandbox(parent, name, service, severity, category, what, yes),
    Command::DeleteCorpus { name, yes } => run_delete_corpus(name, yes),
    Command::TuneDb => println!("{}", bootstrap::db_tuning_guidance()),
    Command::SetAdminToken {
      token,
      generate,
      owner,
    } => run_set_admin_token(token, generate, owner),
    Command::ExportDataset {
      corpus,
      service,
      out,
      group_by,
      severity,
    } => run_export_dataset(corpus, service, out, group_by, severity),
  }
}

/// Prints the `(corpus, service)` conversion-status overview — the CLI surface of the same data the
/// web report top + agent `GET /api/reports/<c>/<s>` show (via the shared
/// `Backend::progress_report`). `--json` mirrors the agent `ServiceOverviewDto`. Exits `1` on an
/// unknown corpus/service.
fn run_report(corpus_name: String, service_name: String, json: bool) {
  let mut backend = backend::from_address(default_db_address());
  let corpus = match Corpus::find_by_name(&corpus_name.to_lowercase(), &mut backend.connection) {
    Ok(corpus) => corpus,
    Err(_) => {
      eprintln!("No such corpus: {corpus_name}");
      std::process::exit(1);
    },
  };
  let service = match Service::find_by_name(&service_name.to_lowercase(), &mut backend.connection) {
    Ok(service) => service,
    Err(_) => {
      eprintln!("No such service: {service_name}");
      std::process::exit(1);
    },
  };
  let stats = backend.progress_report(&corpus, &service);
  let count = |key: &str| stats.get(key).copied().unwrap_or(0.0) as i64;
  let percent = |key: &str| stats.get(&format!("{key}_percent")).copied().unwrap_or(0.0);
  let total = count("total");
  if json {
    let statuses: Vec<_> = TaskStatus::keys()
      .into_iter()
      .map(
        |key| serde_json::json!({ "status": key, "tasks": count(&key), "percent": percent(&key) }),
      )
      .collect();
    let overview = serde_json::json!({
      "corpus": corpus.name,
      "service": service.name,
      "total": total,
      "statuses": statuses,
    });
    println!(
      "{}",
      serde_json::to_string_pretty(&overview).unwrap_or_default()
    );
  } else {
    println!("{} / {}  —  {total} valid tasks", corpus.name, service.name);
    for key in TaskStatus::keys() {
      println!("  {:<12} {:>10}  ({:.2}%)", key, count(&key), percent(&key));
    }
  }
}

/// Prints the run history for a `(corpus, service)` — the CLI surface of the web run-history screen
/// and the agent `GET /api/runs/<c>/<s>`, via `HistoricalRun::find_by` then `with_live_tallies`
/// (live for the open run). `--json` mirrors the agent `RunDto` list. Exits `1` on an unknown pair.
fn run_runs(corpus_name: String, service_name: String, json: bool) {
  let mut backend = backend::from_address(default_db_address());
  let corpus = match Corpus::find_by_name(&corpus_name.to_lowercase(), &mut backend.connection) {
    Ok(corpus) => corpus,
    Err(_) => {
      eprintln!("No such corpus: {corpus_name}");
      std::process::exit(1);
    },
  };
  let service = match Service::find_by_name(&service_name.to_lowercase(), &mut backend.connection) {
    Ok(service) => service,
    Err(_) => {
      eprintln!("No such service: {service_name}");
      std::process::exit(1);
    },
  };
  let stored =
    HistoricalRun::find_by(&corpus, &service, &mut backend.connection).unwrap_or_default();
  let runs: Vec<HistoricalRun> = stored
    .into_iter()
    .map(|run| run.with_live_tallies(&mut backend.connection))
    .collect();
  if json {
    let array: Vec<_> = runs
      .iter()
      .map(|r| {
        serde_json::json!({
          "id": r.id, "owner": r.owner, "description": r.description,
          "start_time": iso(r.start_time), "end_time": r.end_time.map(iso),
          "completed": r.end_time.is_some(), "total": r.total,
          "no_problem": r.no_problem, "warning": r.warning, "error": r.error,
          "fatal": r.fatal, "invalid": r.invalid, "in_progress": r.in_progress,
        })
      })
      .collect();
    println!(
      "{}",
      serde_json::to_string_pretty(&array).unwrap_or_default()
    );
  } else {
    println!(
      "Run history: {} / {}  ({} run(s))",
      corpus.name,
      service.name,
      runs.len()
    );
    for r in &runs {
      let state = if r.end_time.is_some() {
        "completed"
      } else {
        "open"
      };
      println!(
        "  #{}  {}  [{}]  by {}",
        r.id,
        iso(r.start_time),
        state,
        r.owner
      );
      println!(
        "       {} tasks: {} ok · {} warn · {} err · {} fatal · {} inv · {} in-prog",
        r.total, r.no_problem, r.warning, r.error, r.fatal, r.invalid, r.in_progress
      );
      if !r.description.trim().is_empty() {
        println!("       {}", r.description.trim());
      }
    }
  }
}

/// Prints one document's per-article forensics — the CLI surface of the web forensic screen + agent
/// `GET /api/corpus/<c>/<svc>/document/<name>` (via the shared `Task::find_by_name` +
/// `backend::task_messages`). Leads with the status + a severity-count summary, then the actionable
/// messages; info noise is hidden unless `--all`. `--json` mirrors `DocumentReportDto`. Exits `1`
/// on an unknown corpus / service / document.
fn run_document(corpus_name: String, service_name: String, name: String, all: bool, json: bool) {
  let mut backend = backend::from_address(default_db_address());
  let corpus = match Corpus::find_by_name(&corpus_name.to_lowercase(), &mut backend.connection) {
    Ok(corpus) => corpus,
    Err(_) => {
      eprintln!("No such corpus: {corpus_name}");
      std::process::exit(1);
    },
  };
  let service = match Service::find_by_name(&service_name.to_lowercase(), &mut backend.connection) {
    Ok(service) => service,
    Err(_) => {
      eprintln!("No such service: {service_name}");
      std::process::exit(1);
    },
  };
  let task = match Task::find_by_name(&name, &corpus, &service, &mut backend.connection) {
    Ok(task) => task,
    Err(_) => {
      eprintln!(
        "No such document: {name} in {}/{}",
        corpus.name, service.name
      );
      std::process::exit(1);
    },
  };
  let status = TaskStatus::from_raw(task.status);
  // Sampled at backend::DOCUMENT_MESSAGE_CAP per severity; `counts` are the true totals so the
  // summary is accurate even when a pathological document carries millions of messages.
  let (messages, counts) = task_messages(&mut backend.connection, &task);
  let shown = messages.len() as i64;
  let truncated = shown < counts.total();
  if json {
    let msgs: Vec<_> = messages
      .iter()
      .map(|m| {
        serde_json::json!({
          "severity": m.severity(), "category": m.category(), "what": m.what(), "details": m.details(),
        })
      })
      .collect();
    let document = serde_json::json!({
      "corpus": corpus.name,
      "service": service.name,
      "name": name,
      "entry": task.entry.trim_end(),
      "task_id": task.id,
      "status": status.to_key(),
      "status_code": status.raw(),
      "messages": msgs,
      "message_counts": {
        "info": counts.info, "warning": counts.warning, "error": counts.error,
        "fatal": counts.fatal, "invalid": counts.invalid, "total": counts.total(),
      },
      "messages_truncated": truncated,
    });
    println!(
      "{}",
      serde_json::to_string_pretty(&document).unwrap_or_default()
    );
  } else {
    println!(
      "{}  ({}/{})  —  status: {}",
      name,
      corpus.name,
      service.name,
      status.to_key()
    );
    println!(
      "  {} message(s): {} fatal · {} error · {} warning · {} invalid · {} info",
      counts.total(),
      counts.fatal,
      counts.error,
      counts.warning,
      counts.invalid,
      counts.info,
    );
    if truncated {
      println!(
        "  (showing a sample of {shown}; this document has {} messages total)",
        counts.total()
      );
    }
    for message in &messages {
      if message.severity() == "info" && !all {
        continue;
      }
      println!(
        "  {:<8} {:<18} {:<28} {}",
        message.severity(),
        message.category(),
        message.what(),
        message.details()
      );
    }
    if counts.info > 0 && !all {
      println!(
        "  … {} info message(s) hidden — use --all to show",
        counts.info
      );
    }
  }
}

/// Marks a filtered scope for reconversion — the CLI surface of the web/agent rerun, via the shared
/// `Backend::mark_rerun` (resets the matching tasks to TODO for the dispatcher). Dry-run by
/// default; `--yes` executes. Exits `1` on an unknown corpus/service, `2` on an invalid severity.
#[allow(clippy::too_many_arguments)]
fn run_rerun(
  corpus_name: String,
  service_name: String,
  severity: Option<String>,
  category: Option<String>,
  what: Option<String>,
  description: Option<String>,
  owner: String,
  yes: bool,
) {
  let mut backend = backend::from_address(default_db_address());
  let corpus = match Corpus::find_by_name(&corpus_name.to_lowercase(), &mut backend.connection) {
    Ok(corpus) => corpus,
    Err(_) => {
      eprintln!("No such corpus: {corpus_name}");
      std::process::exit(1);
    },
  };
  let service = match Service::find_by_name(&service_name.to_lowercase(), &mut backend.connection) {
    Ok(service) => service,
    Err(_) => {
      eprintln!("No such service: {service_name}");
      std::process::exit(1);
    },
  };
  if let Some(sev) = &severity {
    if TaskStatus::from_key(sev).is_none() {
      eprintln!("Invalid --severity {sev:?} (use no_problem, warning, error, fatal, invalid)");
      std::process::exit(2);
    }
  }
  let mut scope = format!("{}/{}", corpus.name, service.name);
  if let Some(sev) = &severity {
    scope.push_str(&format!("  severity={sev}"));
  }
  if let Some(cat) = &category {
    scope.push_str(&format!("  category={cat}"));
  }
  if let Some(w) = &what {
    scope.push_str(&format!("  what={w}"));
  }

  if !yes {
    println!("Dry run — would mark for reconversion:");
    println!("  {scope}");
    println!("Pass --yes to execute (resets the matching tasks to TODO for the dispatcher).");
    return;
  }
  let options = RerunOptions {
    corpus: &corpus,
    service: &service,
    severity_opt: severity,
    category_opt: category,
    what_opt: what,
    owner_opt: Some(owner),
    description_opt: Some(description.unwrap_or_else(|| "cli rerun".to_string())),
  };
  match backend.mark_rerun(options) {
    Ok(()) => println!("Marked for reconversion: {scope}"),
    Err(error) => {
      eprintln!("rerun failed: {error}");
      std::process::exit(1);
    },
  }
}

/// Carves a sandbox corpus from a parent by a message-condition filter — the CLI surface of the
/// web/agent sandbox, via the shared `backend::create_sandbox` (one backend op, three surfaces).
/// Dry-run by default; `--yes` creates. Exits `1` on an unknown parent/service or a taken sandbox
/// name, `2` on an invalid severity.
#[allow(clippy::too_many_arguments)]
fn run_sandbox(
  parent_name: String,
  name: String,
  service_name: String,
  severity: String,
  category: Option<String>,
  what: Option<String>,
  yes: bool,
) {
  let mut backend = backend::from_address(default_db_address());
  let parent = match Corpus::find_by_name(&parent_name.to_lowercase(), &mut backend.connection) {
    Ok(parent) => parent,
    Err(_) => {
      eprintln!("No such corpus: {parent_name}");
      std::process::exit(1);
    },
  };
  let service = match Service::find_by_name(&service_name.to_lowercase(), &mut backend.connection) {
    Ok(service) => service,
    Err(_) => {
      eprintln!("No such service: {service_name}");
      std::process::exit(1);
    },
  };
  if TaskStatus::from_key(&severity).is_none() {
    eprintln!("Invalid --severity {severity:?} (use no_problem, warning, error, fatal, invalid)");
    std::process::exit(2);
  }
  if Corpus::find_by_name(&name.to_lowercase(), &mut backend.connection).is_ok() {
    eprintln!("A corpus named {name:?} already exists — pick a unique sandbox name");
    std::process::exit(1);
  }

  let mut scope = format!("{}/{}  severity={severity}", parent.name, service.name);
  if let Some(cat) = &category {
    scope.push_str(&format!("  category={cat}"));
  }
  if let Some(w) = &what {
    scope.push_str(&format!("  what={w}"));
  }

  if !yes {
    println!("Dry run — would carve sandbox '{name}' from:");
    println!("  {scope}");
    println!(
      "Pass --yes to create the sandbox (a new corpus with one TODO task per matched entry)."
    );
    return;
  }
  let selection = SandboxSelection {
    service_id: service.id,
    severity,
    category,
    what,
  };
  match create_sandbox(
    &mut backend.connection,
    &parent,
    &name.to_lowercase(),
    &selection,
  ) {
    Ok(outcome) => println!(
      "Created sandbox '{}' from '{}' — {} entries captured.",
      outcome.sandbox.name, parent.name, outcome.entry_count
    ),
    Err(error) => {
      eprintln!("sandbox creation failed: {error}");
      std::process::exit(1);
    },
  }
}

/// Deletes a corpus and all dependent rows via the transactional, orphan-free `Corpus::destroy`
/// (the same primitive the web/agent delete uses) — the CLI surface of corpus removal, completing
/// the sandbox lifecycle (create → iterate → delete). Dry-run by default; `--yes` deletes. Exits
/// `1` on an unknown corpus or a delete failure.
fn run_delete_corpus(name: String, yes: bool) {
  let mut backend = backend::from_address(default_db_address());
  let corpus = match Corpus::find_by_name(&name.to_lowercase(), &mut backend.connection) {
    Ok(corpus) => corpus,
    Err(_) => {
      eprintln!("No such corpus: {name}");
      std::process::exit(1);
    },
  };
  let task_count = corpus.task_count(&mut backend.connection).unwrap_or(-1);
  let kind = if corpus.sandbox_id().is_some() {
    "sandbox"
  } else {
    "corpus"
  };

  if !yes {
    println!(
      "Dry run — would delete {kind} '{}' and all its tasks + log messages:",
      corpus.name
    );
    println!("  {task_count} tasks (historical run tallies are immutable and preserved).");
    println!("Pass --yes to delete.");
    return;
  }
  match corpus.destroy(&mut backend.connection) {
    Ok(_) => println!("Deleted {kind} '{name}' ({task_count} tasks removed)."),
    Err(error) => {
      eprintln!("delete failed: {error}");
      std::process::exit(1);
    },
  }
}

fn run_export_dataset(
  corpus_name: String,
  service_name: String,
  out: PathBuf,
  group_by: String,
  severity: Vec<String>,
) {
  let group_by = match GroupBy::from_key(&group_by) {
    Some(group_by) => group_by,
    None => {
      eprintln!("error: --group-by must be 'month' or 'severity' (got {group_by:?})");
      std::process::exit(2);
    },
  };
  let severities: Vec<TaskStatus> = match severity
    .iter()
    .map(|key| TaskStatus::from_key(key).ok_or_else(|| key.clone()))
    .collect()
  {
    Ok(severities) => severities,
    Err(bad) => {
      eprintln!("error: unknown severity {bad:?} (use no_problem, warning, error, fatal, invalid)");
      std::process::exit(2);
    },
  };

  let mut backend = backend::from_address(default_db_address());
  let corpus = match Corpus::find_by_name(&corpus_name.to_lowercase(), &mut backend.connection) {
    Ok(corpus) => corpus,
    Err(error) => {
      eprintln!("error: corpus {corpus_name:?} not found: {error}");
      std::process::exit(1);
    },
  };
  let service = match Service::find_by_name(&service_name.to_lowercase(), &mut backend.connection) {
    Ok(service) => service,
    Err(error) => {
      eprintln!("error: service {service_name:?} not found: {error}");
      std::process::exit(1);
    },
  };

  println!(
    "Exporting {} / {} → {} (by {})",
    corpus.name,
    service.name,
    out.display(),
    group_by_label(group_by),
  );
  match export_html_dataset(
    &mut backend.connection,
    &corpus,
    &service,
    &severities,
    group_by,
    &out,
    |line| println!("{line}"),
  ) {
    Ok(outcome) => print_export_summary(&outcome),
    Err(error) => {
      eprintln!("cortex export-dataset failed: {error}");
      std::process::exit(1);
    },
  }
}

fn group_by_label(group_by: GroupBy) -> &'static str {
  match group_by {
    GroupBy::Month => "month",
    GroupBy::Severity => "severity",
  }
}

fn print_export_summary(outcome: &DatasetExportOutcome) {
  println!(
    "\nDone: {} archive(s), {} document(s) bundled, {} skipped.",
    outcome.archives.len(),
    outcome.total_entries,
    outcome.skipped,
  );
}

fn run_set_admin_token(token: Option<String>, generate: bool, owner: String) {
  let token = match (generate, token) {
    (true, _) => bootstrap::generate_token(),
    (false, Some(token)) if !token.is_empty() => token,
    (false, _) => {
      eprintln!("error: provide a <TOKEN> argument, or pass --generate to create one");
      std::process::exit(2);
    },
  };
  match bootstrap::set_admin_token(&config_file_path(), &token, &owner) {
    Ok(outcome) => {
      println!(
        "{} admin token for owner '{}' in {} ({} token(s) configured).",
        if outcome.replaced { "Updated" } else { "Added" },
        owner,
        config_file_path().display(),
        outcome.token_count,
      );
      if generate {
        println!("\n  token: {token}\n  (store it now — it is shown only once)");
      }
      if outcome.shadowed_by_legacy_json {
        eprintln!(
          "\nWARNING: a legacy config.json in this directory overrides [auth] in cortex.toml, so \
           this token will NOT take effect until you move its rerun_tokens into cortex.toml (or \
           remove config.json)."
        );
      }
    },
    Err(error) => {
      eprintln!("cortex set-admin-token failed: {error}");
      std::process::exit(1);
    },
  }
}

fn run_init() {
  match bootstrap::init(default_db_address(), &config_file_path()) {
    Ok(outcome) => {
      if outcome.migrations_applied.is_empty() {
        println!("Database already up to date (no migrations applied).");
      } else {
        println!("Applied {} migration(s):", outcome.migrations_applied.len());
        for migration in &outcome.migrations_applied {
          println!("  + {migration}");
        }
      }
      if outcome.config_created {
        println!("Scaffolded config at {}", config_file_path().display());
      }
      println!();
      let report = bootstrap::doctor(default_db_address());
      // `print_doctor_text` already lists actionable next steps (incl. creating an admin token)
      // from `report.remediations()`, so we don't duplicate the token nudge here.
      print_doctor_text(&report);
      println!(
        "\nNext step — tune PostgreSQL for this host:\n{}",
        bootstrap::db_tuning_guidance()
      );
      if !report.ok {
        std::process::exit(1);
      }
    },
    Err(error) => {
      eprintln!("cortex init failed: {error}");
      std::process::exit(1);
    },
  }
}

fn run_doctor(json: bool) {
  let report = bootstrap::doctor(default_db_address());
  if json {
    // Augment the serialized report with the same remediation hints the text output prints, so the
    // agent twin is told *how* to fix a red check, not just that it is red (symmetry).
    let mut value = serde_json::to_value(&report).unwrap_or_default();
    if let Some(object) = value.as_object_mut() {
      object.insert(
        "remediations".to_string(),
        serde_json::json!(report.remediations()),
      );
    }
    println!(
      "{}",
      serde_json::to_string_pretty(&value).unwrap_or_default()
    );
  } else {
    print_doctor_text(&report);
  }
  if !report.ok {
    std::process::exit(1);
  }
}

fn print_doctor_text(report: &DoctorReport) {
  let mark = |ok: bool| if ok { "ok" } else { "FAIL" };
  println!("CorTeX doctor:");
  println!("  [{}] database reachable", mark(report.database_reachable));
  println!("  [{}] migrations current", mark(report.migrations_current));
  println!("  [{}] services seeded", mark(report.services_seeded));
  // Informational (does not affect `=> healthy`): no token just means nobody can sign in yet.
  println!(
    "  [{}] admin token configured",
    if report.admin_token_configured {
      "ok"
    } else {
      "--"
    }
  );
  println!("  => {}", if report.ok { "healthy" } else { "DEGRADED" });
  // Guide the operator from each red / unconfigured check to its fix.
  let remediations = report.remediations();
  if !remediations.is_empty() {
    println!("\nNext steps:");
    for hint in &remediations {
      println!("  → {hint}");
    }
  }
}
