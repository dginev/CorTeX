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
}

fn main() {
  match Cli::parse().command {
    Command::Init => run_init(),
    Command::Doctor { json } => run_doctor(json),
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
      print_doctor_text(&report);
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
    println!(
      "{}",
      serde_json::to_string_pretty(&report).unwrap_or_default()
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
  println!("  => {}", if report.ok { "healthy" } else { "DEGRADED" });
}
