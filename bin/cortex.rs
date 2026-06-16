// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! `cortex` — the administration CLI. A thin renderer over the library: self-install (`init`),
//! diagnostics (`doctor`), DB tuning, token management, the `report` query surface (the CLI twin of
//! the web/agent service overview), and dataset export.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use cortex::backend::{
  self, default_db_address, export_html_dataset, DatasetExportOutcome, GroupBy,
};
use cortex::bootstrap::{self, DoctorReport};
use cortex::config::config_file_path;
use cortex::helpers::TaskStatus;
use cortex::models::{Corpus, Service};

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
