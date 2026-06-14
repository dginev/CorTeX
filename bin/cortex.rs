// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! `cortex` — the administration CLI. A thin renderer over [`cortex::bootstrap`]: self-install
//! (`init`) and diagnostics (`doctor`).

use clap::{Parser, Subcommand};

use cortex::backend::default_db_address;
use cortex::bootstrap::{self, DoctorReport};
use cortex::config::config_file_path;

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
}

fn main() {
  match Cli::parse().command {
    Command::Init => run_init(),
    Command::Doctor { json } => run_doctor(json),
    Command::TuneDb => println!("{}", bootstrap::db_tuning_guidance()),
    Command::SetAdminToken {
      token,
      generate,
      owner,
    } => run_set_admin_token(token, generate, owner),
  }
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
