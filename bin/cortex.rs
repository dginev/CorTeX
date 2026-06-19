// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! `cortex` — the administration CLI. A thin renderer over the library: self-install (`init`),
//! diagnostics (`doctor`), DB tuning, token management, the full corpus/service lifecycle
//! (`create-service`/`delete-service`, `import`/`extend`, `activate`/`deactivate`, `delete-corpus`,
//! `sandbox`), the `report`/`runs`/`document` read surface (the CLI twins of the web/agent report
//! ladder, run-history, and per-article forensics — `report` drills overview → severity → category
//! → `what` → affected documents), the `snapshot`/`rerun` campaign actions, and dataset export.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use cortex::backend::{
  self, DatasetExportOutcome, GroupBy, RerunOptions, SandboxSelection, TaskReportOptions,
  create_sandbox, default_db_address, export_html_dataset, list_task_diffs, summary_task_diffs,
  task_messages,
};
use cortex::bootstrap::{self, DoctorReport};
use cortex::config::config_file_path;
use cortex::frontend::audit::AuditDto;
use cortex::frontend::corpora::CorpusDto;
use cortex::frontend::helpers::group_thousands;
use cortex::frontend::jobs::JobDto;
use cortex::frontend::params::{MAX_REPORT_OFFSET, MAX_REPORT_PAGE_SIZE};
use cortex::frontend::reports::is_valid_rerun_severity;
use cortex::frontend::services::ServiceDto;
use cortex::helpers::TaskStatus;
use cortex::importer::Importer;
use cortex::models::{
  AuditEntry, Corpus, DiffStatusFilter, HistoricalRun, NewCorpus, NewService, Service, Session,
  Task, WorkerMetadata,
};

/// Formats a timestamp the same way the web/agent surfaces do (RFC 3339, seconds) so the CLI's run
/// JSON matches `RunDto`.
fn iso(time: chrono::NaiveDateTime) -> String {
  time
    .and_utc()
    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[derive(Parser)]
#[command(
  name = "cortex",
  version,
  about = "CorTeX administration CLI",
  after_help = "Most commands are the CLI twin of a web screen and a /api endpoint (same backend, \
                same live + historical state) — pick whichever surface fits. Run `cortex <command> \
                --help` for the full description of any command. Full operator guide: MANUAL.md \
                (§14 = the CLI). Consequential mutations (rerun, sandbox, deactivate, delete-*) are \
                dry-run by default — pass `--yes` to execute."
)]
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
  /// Operational snapshot of live activity — backlog, worker fleet, jobs, latest run.
  ///
  /// The CLI twin of the admin dashboard's live-ops console: pending-task backlog, worker fleet,
  /// background jobs, and the latest run. (`doctor` checks the install is healthy; `status` shows
  /// what's happening now.)
  Status {
    /// Emit JSON (the same shape as the admin `/admin/status.json` feed) instead of a text
    /// summary.
    #[arg(long)]
    json: bool,
  },
  /// List recent background jobs with health, progress, and heartbeat age.
  ///
  /// Imports, reruns, reindex/analyze, sandbox carves — the CLI twin of the `/jobs` dashboard and
  /// the agent `GET /api/jobs`. (`status` shows job *counts*; `jobs` shows the list.)
  Jobs {
    /// Show only pending/running jobs (omit terminal ones).
    #[arg(long)]
    active: bool,
    /// Max jobs to list (default 50, capped at 200).
    #[arg(long)]
    limit: Option<i64>,
    /// Emit JSON (the same shape as the agent `JobDto` list) instead of a text table.
    #[arg(long)]
    json: bool,
  },
  /// Review the accountability audit log — who did what, when.
  ///
  /// The CLI twin of the `/admin/audit` screen and the agent `GET /api/audit`. Every mutating
  /// admin action (rerun, import, delete, config change, …) is recorded with its actor +
  /// outcome.
  Audit {
    /// Restrict to a single actor (the owner credited for the action).
    #[arg(long)]
    actor: Option<String>,
    /// Max entries to list, most-recent first (default 50, capped at 500).
    #[arg(long)]
    limit: Option<i64>,
    /// Emit JSON (the same shape as the agent `AuditDto` list) instead of a text table.
    #[arg(long)]
    json: bool,
  },
  /// List all registered corpora (handles, names, document counts, paths).
  ///
  /// The CLI twin of the overview screen and the agent `GET /api/corpora`, sharing the
  /// `CorpusDto`. Shows each corpus's stable `public_id` handle, name, ingested-document count,
  /// and path — the handles/names the other `cortex` subcommands take as input (so this is how
  /// you discover them). `--json` mirrors the agent `CorpusDto` list.
  Corpora {
    /// Emit JSON (the same shape as the agent `CorpusDto` list) instead of a text table.
    #[arg(long)]
    json: bool,
  },
  /// List the service registry (names, versions, input→output formats).
  ///
  /// The CLI twin of the registry screen and the agent `GET /api/services`, sharing the
  /// `ServiceDto`. Shows each service's `public_id`, name, version, and input→output formats
  /// (incl. the magic `init`/`import` services). `--json` mirrors the agent `ServiceDto` list.
  Services {
    /// Emit JSON (the same shape as the agent `ServiceDto` list) instead of a text table.
    #[arg(long)]
    json: bool,
  },
  /// Print PostgreSQL server-tuning guidance for this host (pgtune inputs; see docs/DB_TUNING.md).
  TuneDb,
  /// Print the generated OpenAPI 3 spec for the agent API to stdout.
  ///
  /// The static twin of the live `GET /api/openapi.json`: built straight from the `#[openapi]`
  /// route definitions, so it needs **no running server or database** and can never drift from the
  /// served API. Use it to publish the API docs (the gh-pages docs site bundles it via
  /// `scripts/build-docs-site.sh`) or to feed an OpenAPI client generator —
  /// `cortex openapi > openapi.json`.
  Openapi,
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
  /// Revoke an admin/API token from cortex.toml's [auth] — the inverse of set-admin-token.
  ///
  /// Pass a `<TOKEN>` to revoke that one, or `--owner <name>` to revoke every token attributed to
  /// an owner (e.g. when a person leaves). A revoked token stops working immediately.
  RevokeToken {
    /// The token value to revoke. Omit and pass --owner to revoke all of an owner's tokens.
    token: Option<String>,
    /// Revoke every token attributed to this owner, instead of a single token value.
    #[arg(long)]
    owner: Option<String>,
  },
  /// Print the conversion report for a `(corpus, service)`.
  ///
  /// The CLI twin of the web/agent report ladder. With no drill flags: the service overview
  /// (valid-task total + per-status shares, `GET /api/reports/<c>/<s>`). Add `--severity` for the
  /// category breakdown, `--severity --category` for the `what` breakdown, and `--severity
  /// --category --what` for the affected-document list (paper ids to feed `cortex document`) — the
  /// same rollup-backed grains the agent `/api/reports/...` rungs serve.
  Report {
    /// Corpus name.
    corpus: String,
    /// Service name (e.g. tex_to_html).
    service: String,
    /// Drill into a severity (`warning`|`error`|`fatal`|`invalid`|`info`): the category breakdown.
    #[arg(long)]
    severity: Option<String>,
    /// Drill into a category within the severity: the `what` breakdown (requires `--severity`).
    #[arg(long, requires = "severity")]
    category: Option<String>,
    /// Drill into a `what` within the category: the affected-document list (requires
    /// `--category`).
    #[arg(long, requires = "category")]
    what: Option<String>,
    /// Page offset for the drill-down rungs (default 0).
    #[arg(long)]
    offset: Option<i64>,
    /// Max rows for the drill-down rungs (default 100, capped at 1000).
    #[arg(long)]
    limit: Option<i64>,
    /// Emit JSON (matching the corresponding agent report DTO for the chosen rung) instead of
    /// text.
    #[arg(long)]
    json: bool,
  },
  /// Run history for a `(corpus, service)` — per-run severity tallies over time.
  ///
  /// The CLI twin of the web run-history screen + agent `GET /api/runs/<c>/<s>`: each conversion
  /// run with its per-severity tallies (live for the open run). The macro view of how conversion
  /// quality moved over time.
  Runs {
    /// Corpus name.
    corpus: String,
    /// Service name (e.g. tex_to_html).
    service: String,
    /// Emit JSON (the same shape as the agent `RunDto` list) instead of a text table.
    #[arg(long)]
    json: bool,
  },
  /// Compare two saved task-status snapshots of a `(corpus, service)`.
  ///
  /// The run-diff summary (the CLI twin of the web `/runs/<c>/<s>/diff` screen + agent
  /// `GET /api/runs/<c>/<s>/diff`): the status-transition matrix — how many tasks moved between
  /// severities. With no `--previous`/`--current`, compares the most recent saved pair. Take a
  /// baseline with `cortex snapshot` before a rerun campaign, then `cortex diff` to quantify what
  /// moved.
  Diff {
    /// Corpus name.
    corpus: String,
    /// Service name (e.g. tex_to_html).
    service: String,
    /// Earlier snapshot timestamp (`YYYY-MM-DD HH:MM:SS`, from the listed available dates). Omit
    /// for the most recent saved pair.
    #[arg(long)]
    previous: Option<String>,
    /// Later snapshot timestamp. Omit for the most recent saved pair.
    #[arg(long)]
    current: Option<String>,
    /// Drill into the individual entries that changed status (the per-task list) instead of the
    /// summary matrix — the CLI twin of the web `/runs/<c>/<s>/tasks` screen + agent
    /// `GET /api/runs/<c>/<s>/tasks`. The `--*-status`/`--offset`/`--limit` flags apply here.
    #[arg(long)]
    tasks: bool,
    /// With `--tasks`: only entries whose EARLIER snapshot had this severity
    /// (`no_problem`/`warning`/`error`/`fatal`/`invalid`/`todo`).
    #[arg(long)]
    previous_status: Option<String>,
    /// With `--tasks`: only entries whose LATER snapshot has this severity.
    #[arg(long)]
    current_status: Option<String>,
    /// With `--tasks`: pagination offset (default 0).
    #[arg(long)]
    offset: Option<usize>,
    /// With `--tasks`: page size (default 100, capped).
    #[arg(long)]
    limit: Option<usize>,
    /// Emit JSON (the agent `RunDiffDto` shape, or a `TaskDiffDto` list with `--tasks`) instead of
    /// a text table.
    #[arg(long)]
    json: bool,
  },
  /// Per-article forensics for one document — its status + every log message.
  ///
  /// The CLI twin of the web forensic screen + agent `GET /api/corpus/<c>/<svc>/document/<name>`:
  /// the document's status + every worker-log message.
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
  /// Freeze the current per-task statuses of a `(corpus, service)` into history.
  ///
  /// Writes `historical_tasks` — the CLI twin of the web/agent save-snapshot
  /// (`POST /api/corpora/<c>/services/<s>/snapshot`). Capture a baseline before a rerun campaign,
  /// then diff against it with `cortex runs` / `/runs/.../tasks`. Append-only (history stays
  /// immutable); executes directly (non-destructive).
  Snapshot {
    /// Corpus name.
    corpus: String,
    /// Service name (e.g. tex_to_html).
    service: String,
    /// Emit JSON (the same shape as the agent `SnapshotAckDto`) instead of a text line.
    #[arg(long)]
    json: bool,
  },
  /// Pause a `(corpus, service)` run — block in-progress tasks from being leased.
  ///
  /// Blocks every in-progress task (`status >= 0`) so the dispatcher stops leasing them. CLI twin
  /// of the report screen's "Pause run" and `POST /api/reports/<c>/<s>/pause`. Reversible with
  /// `cortex resume`.
  Pause {
    /// Corpus name.
    corpus: String,
    /// Service name (e.g. tex_to_html).
    service: String,
    /// Emit JSON (corpus/service/action/affected) instead of a text line.
    #[arg(long)]
    json: bool,
  },
  /// Resume a paused `(corpus, service)` run — return Blocked tasks to TODO.
  ///
  /// Returns every Blocked task to TODO so the dispatcher picks them up. CLI twin of "Resume run"
  /// and `POST /api/reports/<c>/<s>/resume`.
  Resume {
    /// Corpus name.
    corpus: String,
    /// Service name (e.g. tex_to_html).
    service: String,
    /// Emit JSON (corpus/service/action/affected) instead of a text line.
    #[arg(long)]
    json: bool,
  },
  /// Mark a filtered scope of a `(corpus, service)` for reconversion.
  ///
  /// The CLI twin of the web/agent rerun. Resets the matching tasks to TODO so the dispatcher
  /// re-runs them. **Dry-run by default** (prints the scope); pass `--yes` to execute.
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
  /// Carve a sandbox corpus out of a parent by a message-condition filter.
  ///
  /// The CLI twin of the web/agent sandbox. A sandbox is a first-class corpus (its own
  /// tasks/runs/reports) you can then run/rerun to iterate a campaign on a subset. Dry-run by
  /// default; pass `--yes` to create.
  Sandbox {
    /// Parent corpus to carve from.
    parent: String,
    /// Name for the new sandbox corpus (must be unique).
    name: String,
    /// Service whose conversion results are filtered (e.g. tex_to_html).
    #[arg(long)]
    service: String,
    /// Filter by task status (`no_problem`|`warning`|`error`|`fatal`|`invalid`) — intersected with
    /// the message filter; supply at least one of the two.
    #[arg(long)]
    status: Option<String>,
    /// Filter by message severity (`info`|`warning`|`error`|`fatal`|`invalid`) — tasks that
    /// emitted such a message, at any status. `--category`/`--what` narrow within it.
    #[arg(long)]
    message_severity: Option<String>,
    /// Restrict to a message category (needs `--message-severity`).
    #[arg(long)]
    category: Option<String>,
    /// Restrict to a message `what` within the category.
    #[arg(long)]
    what: Option<String>,
    /// Restrict to entries whose path contains this substring (e.g. `2506.` for one arXiv month).
    #[arg(long)]
    entry: Option<String>,
    /// Cap the carve at the first N entries (by entry order) — a deterministic size limit.
    #[arg(long)]
    max_entries: Option<i64>,
    /// Actually create the sandbox (without this, the command is a dry run that only prints the
    /// scope).
    #[arg(long)]
    yes: bool,
  },
  /// Register a corpus and import its documents.
  ///
  /// The CLI twin of the web "Add a corpus" form and the agent `POST /api/corpora`. Walks the
  /// corpus path creating one import task per document, so the dispatcher can then convert them
  /// (the usual first step after `cortex init`). Runs synchronously to completion (the web/agent
  /// run it as a background job); exits `1` on a name clash, an unreadable path, or an import
  /// error.
  Import {
    /// Corpus name — the unique handle used everywhere else.
    name: String,
    /// Filesystem path to the corpus root (must be a readable directory on this host).
    path: String,
    /// Documents are multi-file (complex) rather than a single TeX file.
    #[arg(long)]
    complex: bool,
    /// Optional human-readable description.
    #[arg(long, default_value = "")]
    description: String,
  },
  /// Re-scan a corpus's path for newly-arrived documents and import them.
  ///
  /// The CLI twin of the corpus screen's "Extend" button and the agent
  /// `POST /api/corpora/<name>/extend`. Adds an import task per new document and extends each
  /// already-active service to cover them (so new documents get conversion tasks), leaving
  /// existing tasks + results untouched. The incremental companion to `import` for a growing
  /// corpus.
  Extend {
    /// Corpus name to re-scan.
    corpus: String,
  },
  /// Define a new conversion service in the registry.
  ///
  /// The CLI twin of the registry screen's "Register a service" form and the agent
  /// `POST /api/services`. This *defines* a service (e.g. tex_to_html); activating it on a corpus
  /// (creating tasks) is `cortex activate`. The step a fresh-box deploy needs before `activate`,
  /// since only the built-in init/import services are seeded. Exits `1` if the name already
  /// exists.
  CreateService {
    /// Service name — the handle used in `activate`/`report`/etc.
    name: String,
    /// Expected input format (e.g. tex).
    #[arg(long)]
    inputformat: String,
    /// Produced output format (e.g. html).
    #[arg(long)]
    outputformat: String,
    /// Service version.
    #[arg(long, default_value_t = 0.1)]
    version: f32,
    /// The service needs more than a document's main textual content (mark if unsure).
    #[arg(long)]
    complex: bool,
    /// Prerequisite input-conversion service, if any (a pipeline dependency).
    #[arg(long)]
    inputconverter: Option<String>,
    /// Optional human-readable description.
    #[arg(long, default_value = "")]
    description: String,
  },
  /// Activate a service on a corpus (create its conversion tasks).
  ///
  /// The CLI twin of the corpus screen's "Register a service" form and the agent
  /// `POST /api/corpora/<c>/services/<s>`. Creates one TODO task per imported document so the
  /// dispatcher converts them (the step after `import`). Refuses an already-activated pair (use
  /// `rerun` to re-process) and the infrastructure init/import services.
  Activate {
    /// Corpus name.
    corpus: String,
    /// Service name to activate (e.g. tex_to_html).
    service: String,
    /// Owner credited for the activation run (audit identity).
    #[arg(long, default_value = "admin")]
    owner: String,
    /// Description recorded for the activation run (audit trail).
    #[arg(long, default_value = "activated via cortex CLI")]
    description: String,
  },
  /// Deactivate (retire) a service from a corpus.
  ///
  /// The CLI twin of the corpus screen's per-service "deactivate" form and the agent
  /// `DELETE /api/corpora/<c>/services/<s>`. Deletes the pair's tasks + log messages for this
  /// corpus (the service definition and its work elsewhere survive; run tallies are immutable).
  /// The inverse of `activate`. Dry-run by default (prints the blast radius); pass `--yes` to
  /// deactivate.
  Deactivate {
    /// Corpus name.
    corpus: String,
    /// Service name to retire from the corpus.
    service: String,
    /// Actually deactivate (without this, the command is a dry run that only prints the blast
    /// radius).
    #[arg(long)]
    yes: bool,
  },
  /// Delete a corpus (or sandbox) and all of its tasks + log messages.
  ///
  /// The CLI twin of the web/agent `DELETE /api/corpora/<name>`, via the transactional,
  /// orphan-free `Corpus::destroy`. Dry-run by default (prints the blast radius); pass `--yes`
  /// to delete. Historical run tallies are immutable and survive.
  DeleteCorpus {
    /// Corpus name to delete.
    name: String,
    /// Actually delete (without this, the command is a dry run that only prints the blast radius).
    #[arg(long)]
    yes: bool,
  },
  /// Delete a service definition and ALL of its work across every corpus.
  ///
  /// The inverse of `create-service` and the CLI twin of the agent `DELETE /api/services/<name>`,
  /// via the transactional, orphan-free `Service::destroy`. Refuses the infrastructure init/import
  /// services. Dry-run by default (prints the blast radius); pass `--yes` to delete. Run tallies
  /// are immutable and survive.
  DeleteService {
    /// Service name to delete from the registry.
    name: String,
    /// Actually delete (without this, the command is a dry run that only prints the blast radius).
    #[arg(long)]
    yes: bool,
  },
  /// Bundle a corpus/service's converted HTML into ZIP datasets.
  ///
  /// The CLI twin of the web Export dataset screen (`/export/<c>/<s>`) and the agent
  /// `POST /api/corpora/<c>/services/<s>/export-dataset` (replaces the bundle-html-dataset*.sh
  /// scripts). Reads existing result archives off the filesystem (no conversion); resumable;
  /// writes a `<corpus>-manifest.json` provenance sidecar alongside the archives.
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
    /// Optional per-archive size cap in MB: split each bucket into numbered chunks
    /// `<corpus>-<key>-NNN.zip` once it exceeds this many MB of (uncompressed) HTML. Omit for one
    /// archive per bucket (no size limit).
    #[arg(long)]
    max_archive_mb: Option<u64>,
  },
}

fn main() {
  match Cli::parse().command {
    Command::Init => run_init(),
    Command::Doctor { json } => run_doctor(json),
    Command::Status { json } => run_status(json),
    Command::Jobs {
      active,
      limit,
      json,
    } => run_jobs(active, limit, json),
    Command::Audit { actor, limit, json } => run_audit(actor, limit, json),
    Command::Corpora { json } => run_corpora(json),
    Command::Services { json } => run_services(json),
    Command::Report {
      corpus,
      service,
      severity,
      category,
      what,
      offset,
      limit,
      json,
    } => run_report(ReportArgs {
      corpus,
      service,
      severity,
      category,
      what,
      offset,
      limit,
      json,
    }),
    Command::Runs {
      corpus,
      service,
      json,
    } => run_runs(corpus, service, json),
    Command::Diff {
      corpus,
      service,
      previous,
      current,
      tasks,
      previous_status,
      current_status,
      offset,
      limit,
      json,
    } => {
      if tasks {
        run_diff_tasks(
          corpus,
          service,
          previous,
          current,
          previous_status,
          current_status,
          offset,
          limit,
          json,
        )
      } else {
        run_diff(corpus, service, previous, current, json)
      }
    },
    Command::Snapshot {
      corpus,
      service,
      json,
    } => run_snapshot(corpus, service, json),
    Command::Pause {
      corpus,
      service,
      json,
    } => run_pause_resume(corpus, service, json, true),
    Command::Resume {
      corpus,
      service,
      json,
    } => run_pause_resume(corpus, service, json, false),
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
      status,
      message_severity,
      category,
      what,
      entry,
      max_entries,
      yes,
    } => run_sandbox(
      parent,
      name,
      service,
      status,
      message_severity,
      category,
      what,
      entry,
      max_entries,
      yes,
    ),
    Command::Import {
      name,
      path,
      complex,
      description,
    } => run_import(name, path, complex, description),
    Command::Extend { corpus } => run_extend(corpus),
    Command::CreateService {
      name,
      inputformat,
      outputformat,
      version,
      complex,
      inputconverter,
      description,
    } => run_create_service(
      name,
      inputformat,
      outputformat,
      version,
      complex,
      inputconverter,
      description,
    ),
    Command::Activate {
      corpus,
      service,
      owner,
      description,
    } => run_activate(corpus, service, owner, description),
    Command::Deactivate {
      corpus,
      service,
      yes,
    } => run_deactivate(corpus, service, yes),
    Command::DeleteCorpus { name, yes } => run_delete_corpus(name, yes),
    Command::DeleteService { name, yes } => run_delete_service(name, yes),
    Command::TuneDb => println!("{}", bootstrap::db_tuning_guidance()),
    Command::Openapi => println!("{}", cortex::frontend::apidoc::spec_json()),
    Command::SetAdminToken {
      token,
      generate,
      owner,
    } => run_set_admin_token(token, generate, owner),
    Command::RevokeToken { token, owner } => run_revoke_token(token, owner),
    Command::ExportDataset {
      corpus,
      service,
      out,
      group_by,
      severity,
      max_archive_mb,
    } => run_export_dataset(corpus, service, out, group_by, severity, max_archive_mb),
  }
}

/// Prints the `(corpus, service)` conversion-status overview — the CLI surface of the same data the
/// web report top + agent `GET /api/reports/<c>/<s>` show (via the shared
/// `Backend::progress_report`). `--json` mirrors the agent `ServiceOverviewDto`. Exits `1` on an
/// unknown corpus/service.
/// Inputs for the `report` command's drill-down ladder (struct-passed to keep one argument).
struct ReportArgs {
  /// Corpus name.
  corpus: String,
  /// Service name.
  service: String,
  /// Drill severity; `None` = service overview.
  severity: Option<String>,
  /// Drill category (within the severity).
  category: Option<String>,
  /// Drill `what` (within the category).
  what: Option<String>,
  /// Page offset for the drill rungs.
  offset: Option<i64>,
  /// Page size for the drill rungs.
  limit: Option<i64>,
  /// Emit JSON instead of text.
  json: bool,
}

/// Severities that carry log messages and so have a category/`what` breakdown — matches the agent
/// report endpoints' `is_rollup_severity` (`no_problem` has no messages, so no drill-down).
fn is_drillable_severity(severity: &str) -> bool {
  matches!(severity, "warning" | "error" | "fatal" | "invalid" | "info")
}

/// `cortex report` — the CLI report ladder, the twin of the web `/corpus/...` screens and the agent
/// `GET /api/reports/...` rungs. Dispatches on the drill flags: none → service overview;
/// `severity` → category breakdown; `severity + category` → `what` breakdown; `severity + category
/// + what` → the affected-document list. The breakdown rungs read the same `report_summary` rollup
/// the agent serves; the document list uses the live per-task path (bounded by `--offset/--limit`).
/// Exits `1` on an unknown corpus/service or an un-drillable severity.
fn run_report(args: ReportArgs) {
  let mut backend = backend::from_address(default_db_address());
  let corpus = match Corpus::find_by_name(&args.corpus.to_lowercase(), &mut backend.connection) {
    Ok(corpus) => corpus,
    Err(_) => {
      eprintln!("No such corpus: {}", args.corpus);
      std::process::exit(1);
    },
  };
  let service = match Service::find_by_name(&args.service.to_lowercase(), &mut backend.connection) {
    Ok(service) => service,
    Err(_) => {
      eprintln!("No such service: {}", args.service);
      std::process::exit(1);
    },
  };
  // `clap` `requires` guarantees category⊆severity and what⊆category, so these four arms are the
  // only reachable combinations.
  match (
    args.severity.as_deref(),
    args.category.as_deref(),
    args.what.as_deref(),
  ) {
    (None, _, _) => report_overview(&mut backend, &corpus, &service, args.json),
    (Some(severity), None, _) => {
      report_categories(&mut backend, &corpus, &service, severity, &args)
    },
    (Some(severity), Some(category), None) => {
      report_whats(&mut backend, &corpus, &service, severity, category, &args)
    },
    (Some(severity), Some(category), Some(what)) => report_entries(
      &mut backend,
      &corpus,
      &service,
      severity,
      category,
      what,
      &args,
    ),
  }
}

/// The service overview rung (no drill flags): valid-task total + per-status counts/shares.
fn report_overview(backend: &mut backend::Backend, corpus: &Corpus, service: &Service, json: bool) {
  let stats = backend.progress_report(corpus, service);
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

/// Clamps `--offset/--limit` to the shared report bounds (one source of truth with the web + agent
/// surfaces — an unbounded page or deep offset is a connection-pinning scan).
fn drill_window(args: &ReportArgs) -> (i64, i64) {
  let offset = args.offset.unwrap_or(0).clamp(0, MAX_REPORT_OFFSET);
  let limit = args.limit.unwrap_or(100).clamp(1, MAX_REPORT_PAGE_SIZE);
  (offset, limit)
}

/// On the human (text) drill surfaces, prints a one-line hint when a page came back **full** — so a
/// capped result never silently reads as "that's everything" (the no-silent-caps rule, on the CLI).
/// Silent when the page wasn't full; `--json` callers paginate from `offset`/`page_size`
/// themselves.
fn print_page_hint(shown: usize, offset: i64, page_size: i64) {
  if shown as i64 >= page_size {
    println!(
      "  … page full ({page_size} shown); more may exist — continue with `--offset {}`",
      offset + page_size
    );
  }
}

/// Exits `1` with a helpful message if the severity has no message breakdown.
fn require_drillable(severity: &str) {
  if !is_drillable_severity(severity) {
    eprintln!(
      "Severity {severity:?} has no message breakdown. Drillable: warning, error, fatal, invalid, info."
    );
    std::process::exit(1);
  }
}

/// Prints a breakdown rung (category or `what`): a totals line then one row per grain (`tasks`,
/// `messages`, name), ordered by descending task count — the text twin of the agent's
/// `CategoryReportDto`/`WhatReportDto`.
fn print_breakdown(
  title: &str,
  total_tasks: i64,
  total_messages: i64,
  rows: &[(String, i64, i64)],
) {
  println!("{title}");
  println!(
    "  totals: {} tasks · {} messages   ({} row(s) shown, by task count)",
    group_thousands(total_tasks),
    group_thousands(total_messages),
    rows.len()
  );
  for (name, tasks, messages) in rows {
    let label = if name.is_empty() {
      "(none)"
    } else {
      name.as_str()
    };
    println!(
      "  {:>12} tasks  {:>16} msgs   {label}",
      group_thousands(*tasks),
      group_thousands(*messages)
    );
  }
}

/// The category breakdown rung (`--severity`): one row per message category.
fn report_categories(
  backend: &mut backend::Backend,
  corpus: &Corpus,
  service: &Service,
  severity: &str,
  args: &ReportArgs,
) {
  require_drillable(severity);
  let (offset, limit) = drill_window(args);
  let rows: Vec<(String, i64, i64)> = backend
    .category_rollup(corpus, service, severity, limit, offset)
    .into_iter()
    .map(|row| (row.category, row.task_count, row.message_count))
    .collect();
  let (total_tasks, total_messages) = backend.severity_totals(corpus, service, severity);
  if args.json {
    let categories: Vec<_> = rows
      .iter()
      .map(|(name, tasks, messages)| {
        serde_json::json!({ "name": name, "tasks": tasks, "messages": messages })
      })
      .collect();
    println!(
      "{}",
      serde_json::to_string_pretty(&serde_json::json!({
        "severity": severity, "total_tasks": total_tasks,
        "total_messages": total_messages, "categories": categories,
      }))
      .unwrap_or_default()
    );
  } else {
    print_breakdown(
      &format!(
        "{} / {} — {severity} category breakdown",
        corpus.name, service.name
      ),
      total_tasks,
      total_messages,
      &rows,
    );
    print_page_hint(rows.len(), offset, limit);
  }
}

/// The `what` breakdown rung (`--severity --category`): one row per `what` within the category.
fn report_whats(
  backend: &mut backend::Backend,
  corpus: &Corpus,
  service: &Service,
  severity: &str,
  category: &str,
  args: &ReportArgs,
) {
  require_drillable(severity);
  let (offset, limit) = drill_window(args);
  let rows: Vec<(String, i64, i64)> = backend
    .what_rollup(corpus, service, severity, category, limit, offset)
    .into_iter()
    .map(|row| {
      (
        row.what.unwrap_or_default(),
        row.task_count,
        row.message_count,
      )
    })
    .collect();
  let (total_tasks, total_messages) = backend.category_totals(corpus, service, severity, category);
  if args.json {
    let whats: Vec<_> = rows
      .iter()
      .map(|(name, tasks, messages)| {
        serde_json::json!({ "name": name, "tasks": tasks, "messages": messages })
      })
      .collect();
    println!(
      "{}",
      serde_json::to_string_pretty(&serde_json::json!({
        "severity": severity, "category": category, "total_tasks": total_tasks,
        "total_messages": total_messages, "whats": whats,
      }))
      .unwrap_or_default()
    );
  } else {
    print_breakdown(
      &format!(
        "{} / {} — {severity} / {category} what breakdown",
        corpus.name, service.name
      ),
      total_tasks,
      total_messages,
      &rows,
    );
    print_page_hint(rows.len(), offset, limit);
  }
}

/// The deepest rung (`--severity --category --what`): the affected-document list — paper ids (feed
/// `cortex document`) + task id + the message detail. Live per-task path, bounded by the window;
/// the text twin of the agent's `EntryListDto`.
fn report_entries(
  backend: &mut backend::Backend,
  corpus: &Corpus,
  service: &Service,
  severity: &str,
  category: &str,
  what: &str,
  args: &ReportArgs,
) {
  require_drillable(severity);
  let (offset, page_size) = drill_window(args);
  let rows = backend.task_report(TaskReportOptions {
    corpus,
    service,
    severity_opt: Some(severity.to_string()),
    category_opt: Some(category.to_string()),
    what_opt: Some(what.to_string()),
    all_messages: false,
    offset,
    page_size,
  });
  let entries: Vec<(String, i64, String)> = rows
    .iter()
    .map(|row| {
      (
        row.get("entry_name").cloned().unwrap_or_default(),
        row
          .get("entry_taskid")
          .and_then(|id| id.parse().ok())
          .unwrap_or(0),
        row.get("details").cloned().unwrap_or_default(),
      )
    })
    .collect();
  if args.json {
    let json_entries: Vec<_> = entries
      .iter()
      .map(|(name, task_id, details)| {
        serde_json::json!({ "name": name, "task_id": task_id, "details": details })
      })
      .collect();
    println!(
      "{}",
      serde_json::to_string_pretty(&serde_json::json!({
        "corpus": corpus.name, "service": service.name, "severity": severity,
        "category": category, "what": what, "offset": offset, "page_size": page_size,
        "entries": json_entries,
      }))
      .unwrap_or_default()
    );
  } else {
    println!(
      "{} / {} — {severity} / {category} / {what}: affected documents (offset {offset}, {} shown)",
      corpus.name,
      service.name,
      entries.len()
    );
    for (name, task_id, details) in &entries {
      let label = if name.is_empty() {
        "(unnamed)"
      } else {
        name.as_str()
      };
      if details.trim().is_empty() {
        println!("  {label}  #{task_id}");
      } else {
        println!("  {label}  #{task_id}   {}", details.trim());
      }
    }
    print_page_hint(entries.len(), offset, page_size);
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
    // Newest-first (`find_by` orders `start_time.desc()`), so run `i` is compared against the
    // next-older run `i+1` for the run-over-run delta — the same "how did this run move the
    // conversion tallies" view the web run-history screen renders. Text (human) surface only;
    // `--json` above stays raw (agents diff themselves).
    for (i, r) in runs.iter().enumerate() {
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
      if let Some(older) = runs.get(i + 1) {
        // `{:+}` always shows the sign: `+26 ok` (more clean conversions = better), `-138 fatal`
        // (fewer fatals = better). No older run for the oldest row → no delta line.
        println!(
          "       Δ vs previous: {:+} ok · {:+} warn · {:+} err · {:+} fatal",
          r.no_problem - older.no_problem,
          r.warning - older.warning,
          r.error - older.error,
          r.fatal - older.fatal,
        );
      }
      if !r.description.trim().is_empty() {
        println!("       {}", r.description.trim());
      }
    }
  }
}

/// Parses an optional `--previous`/`--current` snapshot timestamp (the `available_dates` format,
/// `YYYY-MM-DD HH:MM:SS[.f]`). `None`/empty means "let the backend pick the default pair". The raw
/// string is returned as the `Err` so the caller can name it in the error message.
fn parse_cli_snapshot_date(raw: Option<&str>) -> Result<Option<chrono::NaiveDateTime>, String> {
  match raw.map(str::trim).filter(|value| !value.is_empty()) {
    None => Ok(None),
    Some(value) => chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f")
      .map(Some)
      .map_err(|_| value.to_string()),
  }
}

/// Compares two saved task-status snapshots of a `(corpus, service)` — the CLI twin of the web
/// `/runs/<c>/<s>/diff` screen + agent `GET /api/runs/<c>/<s>/diff`, over the shared
/// `summary_task_diffs`. Prints the snapshots available to compare and the (previous → current)
/// status-transition matrix; `--json` mirrors the agent `RunDiffDto` shape exactly.
fn run_diff(
  corpus_name: String,
  service_name: String,
  previous: Option<String>,
  current: Option<String>,
  json: bool,
) {
  let previous_date = match parse_cli_snapshot_date(previous.as_deref()) {
    Ok(date) => date,
    Err(raw) => {
      eprintln!("error: --previous {raw:?} is not a YYYY-MM-DD HH:MM:SS timestamp");
      std::process::exit(2);
    },
  };
  let current_date = match parse_cli_snapshot_date(current.as_deref()) {
    Ok(date) => date,
    Err(raw) => {
      eprintln!("error: --current {raw:?} is not a YYYY-MM-DD HH:MM:SS timestamp");
      std::process::exit(2);
    },
  };

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

  let (available_dates, rows) = summary_task_diffs(
    &mut backend.connection,
    &corpus,
    &service,
    previous_date,
    current_date,
  );

  if json {
    let transitions: Vec<_> = rows
      .iter()
      .map(|row| {
        serde_json::json!({
          "previous_status": row.previous_status,
          "current_status": row.current_status,
          "task_count": row.task_count,
        })
      })
      .collect();
    let dto = serde_json::json!({
      "available_dates": available_dates,
      "transitions": transitions,
    });
    println!("{}", serde_json::to_string_pretty(&dto).unwrap_or_default());
    return;
  }

  println!("Run diff: {} / {}", corpus.name, service.name);
  if available_dates.is_empty() {
    println!(
      "  no saved snapshots yet — take a baseline with `cortex snapshot {} {}`",
      corpus.name, service.name
    );
    return;
  }
  println!(
    "  snapshots available to compare: {}",
    available_dates.join(" · ")
  );
  // The matrix carries every (previous × current) cell of the 4 completed severities, including the
  // unchanged diagonal; only nonzero cells are worth printing. `→` marks a real transition,
  // `=` an unchanged count (stable tasks).
  let moved: Vec<_> = rows.iter().filter(|row| row.task_count > 0).collect();
  if moved.is_empty() {
    println!("  no tasks in the compared snapshots (or no changes to report).");
    return;
  }
  println!("  transitions (previous → current : tasks):");
  for row in moved {
    let arrow = if row.previous_status == row.current_status {
      '='
    } else {
      '→'
    };
    println!(
      "    {:>10} {arrow} {:<10} : {}",
      row.previous_status, row.current_status, row.task_count
    );
  }
}

/// Parses an optional `--previous-status`/`--current-status` severity filter for `cortex diff
/// --tasks`. `None`/empty means "no filter on this side"; an unknown key exits 2 (mirroring the
/// agent's `400`).
fn parse_cli_diff_status(raw: Option<&str>, flag: &str) -> Option<TaskStatus> {
  match raw.map(str::trim).filter(|value| !value.is_empty()) {
    None => None,
    Some(value) => match TaskStatus::from_key(value) {
      Some(status) => Some(status),
      None => {
        eprintln!(
          "error: {flag} {value:?} is not a severity (use no_problem, warning, error, fatal, \
           invalid, todo)"
        );
        std::process::exit(2);
      },
    },
  }
}

/// The `cortex diff --tasks` drill: the individual entries whose status changed between two
/// snapshots — the CLI twin of the web `/runs/<c>/<s>/tasks` screen + agent
/// `GET /api/runs/<c>/<s>/tasks`, over the shared `list_task_diffs`. Optionally filtered to a
/// `previous_status → current_status` transition and paginated (`--offset`/`--limit`, bounded like
/// the report drill). `--json` mirrors the agent `TaskDiffDto` list exactly.
#[allow(clippy::too_many_arguments)]
fn run_diff_tasks(
  corpus_name: String,
  service_name: String,
  previous: Option<String>,
  current: Option<String>,
  previous_status: Option<String>,
  current_status: Option<String>,
  offset: Option<usize>,
  limit: Option<usize>,
  json: bool,
) {
  let previous_date = match parse_cli_snapshot_date(previous.as_deref()) {
    Ok(date) => date,
    Err(raw) => {
      eprintln!("error: --previous {raw:?} is not a YYYY-MM-DD HH:MM:SS timestamp");
      std::process::exit(2);
    },
  };
  let current_date = match parse_cli_snapshot_date(current.as_deref()) {
    Ok(date) => date,
    Err(raw) => {
      eprintln!("error: --current {raw:?} is not a YYYY-MM-DD HH:MM:SS timestamp");
      std::process::exit(2);
    },
  };
  let previous_status = parse_cli_diff_status(previous_status.as_deref(), "--previous-status");
  let current_status = parse_cli_diff_status(current_status.as_deref(), "--current-status");

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

  // Bound offset/page_size exactly like the agent endpoint (R-8 / P-4): never an unpaginated or
  // scan-and-discard task-diff.
  let bounded_offset = offset.unwrap_or(0).min(MAX_REPORT_OFFSET as usize);
  let page_size = limit.unwrap_or(100).clamp(1, MAX_REPORT_PAGE_SIZE as usize);
  let filters = DiffStatusFilter {
    previous_status,
    current_status,
    previous_date,
    current_date,
    offset: bounded_offset,
    page_size,
  };
  let tasks = list_task_diffs(&mut backend.connection, &corpus, &service, filters);

  if json {
    let array: Vec<_> = tasks
      .iter()
      .map(|task| {
        serde_json::json!({
          "task_id": task.task_id,
          "entry": task.entry,
          "previous_status": task.previous_status,
          "current_status": task.current_status,
          "previous_saved_at": task.previous_saved_at,
          "current_saved_at": task.current_saved_at,
        })
      })
      .collect();
    println!(
      "{}",
      serde_json::to_string_pretty(&array).unwrap_or_default()
    );
    return;
  }

  println!(
    "Run task-diff: {} / {}  ({} changed entr{})",
    corpus.name,
    service.name,
    tasks.len(),
    if tasks.len() == 1 { "y" } else { "ies" }
  );
  for task in &tasks {
    println!(
      "  {:>10} → {:<10}  {}",
      task.previous_status, task.current_status, task.entry
    );
  }
  print_page_hint(tasks.len(), bounded_offset as i64, page_size as i64);
}

/// Freezes the current per-task statuses of a `(corpus, service)` into `historical_tasks` — the CLI
/// surface of the web/agent save-snapshot, via the shared `Backend::save_historical_tasks`.
/// Append-only and non-destructive, so it executes directly (no dry-run); `--json` mirrors the
/// agent `SnapshotAckDto`. Exits `1` on an unknown corpus/service or a save error.
fn run_snapshot(corpus_name: String, service_name: String, json: bool) {
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
  let saved = match backend.save_historical_tasks(&corpus, &service) {
    Ok(count) => count,
    Err(error) => {
      eprintln!("Failed to save snapshot: {error}");
      std::process::exit(1);
    },
  };
  if json {
    println!(
      "{}",
      serde_json::to_string_pretty(&serde_json::json!({
        "corpus": corpus.name, "service": service.name, "saved": saved,
      }))
      .unwrap_or_default()
    );
  } else {
    println!(
      "Saved a snapshot of {} task statuses for {} / {} into historical_tasks.",
      group_thousands(saved as i64),
      corpus.name,
      service.name
    );
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

/// Pause/resume a `(corpus, service)` run — the CLI surface of the web/agent run control, via the
/// shared `Backend::{pause_run, resume_run}` (block in-progress tasks / restore blocked tasks to
/// TODO). Executes directly (status-only, reversible); `--json` emits the affected count. Exits `1`
/// on an unknown corpus/service or a DB error.
fn run_pause_resume(corpus_name: String, service_name: String, json: bool, pause: bool) {
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
  let action = if pause { "pause" } else { "resume" };
  let result = if pause {
    backend.pause_run(corpus.id, service.id)
  } else {
    backend.resume_run(corpus.id, service.id)
  };
  let affected = match result {
    Ok(count) => count,
    Err(error) => {
      eprintln!("Failed to {action} run: {error}");
      std::process::exit(1);
    },
  };
  if json {
    println!(
      "{}",
      serde_json::to_string_pretty(&serde_json::json!({
        "corpus": corpus.name, "service": service.name, "action": action, "affected": affected,
      }))
      .unwrap_or_default()
    );
  } else {
    let verb = if pause { "Blocked" } else { "Returned to TODO" };
    println!(
      "{verb} {} task(s) for {} / {} ({action}d the run).",
      group_thousands(affected as i64),
      corpus.name,
      service.name
    );
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
  // Validate the rerun severity with the SAME context-aware rule as the agent + human surfaces
  // (R-9): without `--category` it's a task status; with one it's a message severity.
  if let Some(sev) = &severity
    && !is_valid_rerun_severity(sev, category.is_some())
  {
    eprintln!(
      "Invalid --severity {sev:?} for this rerun: without --category use a task status \
         (no_problem|warning|error|fatal|invalid); with --category use a message severity \
         (warning|error|fatal|invalid|info)."
    );
    std::process::exit(2);
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
  status: Option<String>,
  message_severity: Option<String>,
  category: Option<String>,
  what: Option<String>,
  entry: Option<String>,
  max_entries: Option<i64>,
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
  if Corpus::find_by_name(&name.to_lowercase(), &mut backend.connection).is_ok() {
    eprintln!("A corpus named {name:?} already exists — pick a unique sandbox name");
    std::process::exit(1);
  }

  // Normalize the optional narrowings (blank → none, non-positive cap → none) + build the filter.
  let blank_to_none = |value: Option<String>| value.filter(|text| !text.trim().is_empty());
  let selection = SandboxSelection {
    service_id: service.id,
    status: blank_to_none(status),
    message_severity: blank_to_none(message_severity),
    category: blank_to_none(category),
    what: blank_to_none(what),
    entry: blank_to_none(entry),
    max_entries: max_entries.filter(|n| *n > 0),
    severity: None,
  };
  // Pre-flight the intersecting status/message filters (the same set as the web/agent path).
  if let Err(reason) = selection.validate() {
    eprintln!("Invalid sandbox filter: {reason}");
    std::process::exit(2);
  }

  let scope = format!(
    "{}/{}  {}",
    parent.name,
    service.name,
    selection.filter_summary()
  );
  if !yes {
    println!("Dry run — would carve sandbox '{name}' from:");
    println!("  {scope}");
    println!(
      "Pass --yes to create the sandbox (a new corpus with one TODO task per matched entry)."
    );
    return;
  }
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

/// Deletes a service definition and all of its work across every corpus — the inverse of
/// `create-service`, the CLI surface of the agent `DELETE /api/services/<name>`, via the
/// transactional, orphan-free `Backend::destroy_service_by_name` (which refuses the magic
/// init/import services as defense-in-depth). Dry-run by default (prints the blast radius across
/// all corpora); `--yes` executes. Exits `1` on an unknown service, an infrastructure service, or a
/// delete error.
fn run_delete_service(name: String, yes: bool) {
  let mut backend = backend::from_address(default_db_address());
  let service = match Service::find_by_name(&name.to_lowercase(), &mut backend.connection) {
    Ok(service) => service,
    Err(_) => {
      eprintln!("No such service: {name}");
      std::process::exit(1);
    },
  };
  if service.id <= 2 {
    eprintln!(
      "'{}' is an infrastructure service (init/import) and cannot be deleted.",
      service.name
    );
    std::process::exit(1);
  }
  let task_count = service
    .total_task_count(&mut backend.connection)
    .unwrap_or(-1);

  if !yes {
    println!(
      "Dry run — would delete service '{}' and all of its work across every corpus:",
      service.name
    );
    println!(
      "  {} task(s) + their log messages, on every corpus this service is activated (run tallies are immutable and survive).",
      group_thousands(task_count)
    );
    println!("Pass --yes to delete.");
    return;
  }
  match backend.destroy_service_by_name(&service.name) {
    Ok(_) => println!(
      "Deleted service '{}' ({} task(s) removed across all corpora).",
      service.name,
      group_thousands(task_count)
    ),
    Err(error) => {
      eprintln!("delete failed: {error}");
      std::process::exit(1);
    },
  }
}

/// Registers a corpus and imports its documents synchronously — the CLI surface of the web "Add a
/// corpus" form and the agent `POST /api/corpora`, driving the same `Importer` machinery (one
/// capability across all three surfaces). Pre-flights the path + name like the agent does (so a
/// doomed import is never half-registered), runs the walk to completion, then prints the
/// document count. Exits `1` on a name clash, an unreadable path, or an import error.
fn run_import(name: String, path: String, complex: bool, description: String) {
  let mut backend = backend::from_address(default_db_address());
  // Name must be free (matches the agent's 409).
  if Corpus::find_by_name(&name, &mut backend.connection).is_ok() {
    eprintln!("A corpus named {name:?} already exists.");
    std::process::exit(1);
  }
  // Pre-flight the source path (matches the agent's 422): a doomed import is never started, so we
  // don't leave a registered corpus whose import silently found nothing.
  if !std::fs::metadata(path.trim_end())
    .map(|meta| meta.is_dir())
    .unwrap_or(false)
  {
    eprintln!("Path {path:?} is not a readable directory on this host.");
    std::process::exit(1);
  }
  if let Err(error) = backend.add(&NewCorpus {
    name: name.clone(),
    path: path.clone(),
    complex,
    description,
  }) {
    eprintln!("Could not register the corpus: {error}");
    std::process::exit(1);
  }
  let corpus = match Corpus::find_by_name(&name, &mut backend.connection) {
    Ok(corpus) => corpus,
    Err(error) => {
      eprintln!("Registered the corpus but could not reload it: {error}");
      std::process::exit(1);
    },
  };
  let corpus_id = corpus.id;
  println!("Importing {name} from {path} …");
  let mut importer = Importer {
    corpus,
    backend,
    cwd: Importer::cwd(),
    active_prefixes: std::collections::HashSet::new(),
  };
  if let Err(error) = importer.process() {
    eprintln!("Import failed: {error}");
    std::process::exit(1);
  }
  let imported = Corpus::document_counts(&mut importer.backend.connection)
    .get(&corpus_id)
    .copied()
    .unwrap_or(0);
  println!(
    "Imported {} document(s) into corpus {name}. Start the dispatcher (or activate a service) to convert them.",
    group_thousands(imported)
  );
}

/// Defines a new conversion service in the registry — the CLI surface of the registry screen's
/// "Register a service" form and the agent `POST /api/services`. Defines the service (it does *not*
/// activate it on any corpus — that's `activate`). An empty `--inputconverter` is treated as none.
/// Exits `1` if the name already exists or the insert fails.
#[allow(clippy::too_many_arguments)]
fn run_create_service(
  name: String,
  inputformat: String,
  outputformat: String,
  version: f32,
  complex: bool,
  inputconverter: Option<String>,
  description: String,
) {
  let mut backend = backend::from_address(default_db_address());
  if Service::find_by_name(&name, &mut backend.connection).is_ok() {
    eprintln!("A service named {name:?} already exists.");
    std::process::exit(1);
  }
  if let Err(error) = backend.add(&NewService {
    name: name.clone(),
    version,
    inputformat,
    outputformat,
    // An empty string means "no prerequisite" (matches the agent/registry-form handling).
    inputconverter: inputconverter.filter(|value| !value.is_empty()),
    complex,
    description,
  }) {
    eprintln!("Could not create the service: {error}");
    std::process::exit(1);
  }
  println!(
    "Created service '{name}'. Activate it on a corpus with:  cortex activate <corpus> {name}"
  );
}

/// Re-scans a corpus's path for newly-arrived documents and imports them — the CLI surface of the
/// corpus screen's "Extend" button and the agent `POST /api/corpora/<name>/extend`, driving the
/// same `Importer::extend_corpus` + `Backend::extend_service`. Adds an import task per new document
/// and extends each already-active real service (id > 2) to cover them, leaving existing tasks +
/// results untouched. Prints how many new documents were added. Exits `1` on an unknown corpus or
/// an extend error.
fn run_extend(corpus_name: String) {
  let mut backend = backend::from_address(default_db_address());
  let corpus = match Corpus::find_by_name(&corpus_name.to_lowercase(), &mut backend.connection) {
    Ok(corpus) => corpus,
    Err(_) => {
      eprintln!("No such corpus: {corpus_name}");
      std::process::exit(1);
    },
  };
  let corpus_id = corpus.id;
  let corpus_path = corpus.path.clone();
  // Pre-flight the source path (extend re-scans it): a gone/unreadable mount otherwise silently
  // yields "0 new" (glob over a missing dir is an empty set, not an error). Fail transparently —
  // the same guard the web/agent extend applies (422 there, exit 1 here).
  if !std::path::Path::new(corpus_path.trim_end()).is_dir() {
    eprintln!(
      "error: corpus path {corpus_path:?} is not a readable directory — is the data mount present?"
    );
    std::process::exit(1);
  }
  let before = Corpus::document_counts(&mut backend.connection)
    .get(&corpus_id)
    .copied()
    .unwrap_or(0);
  println!(
    "Extending {} — re-scanning {corpus_path} for new documents …",
    corpus.name
  );
  let mut importer = Importer {
    corpus,
    backend,
    cwd: Importer::cwd(),
    active_prefixes: std::collections::HashSet::new(),
  };
  if let Err(error) = importer.extend_corpus() {
    eprintln!("Extend failed: {error}");
    std::process::exit(1);
  }
  // Extend each already-active real service (id > 2) to the new documents so they get conversion
  // tasks too — the magic init/import services are not "activated" services. A per-service failure
  // is logged but doesn't abort the others.
  let services = importer
    .corpus
    .select_services(&mut importer.backend.connection)
    .unwrap_or_default();
  for service in services.iter().filter(|service| service.id > 2) {
    if let Err(error) = importer.backend.extend_service(service, &corpus_path) {
      eprintln!(
        "Warning: could not extend service {}: {error}",
        service.name
      );
    }
  }
  let after = Corpus::document_counts(&mut importer.backend.connection)
    .get(&corpus_id)
    .copied()
    .unwrap_or(0);
  println!(
    "Extended {corpus_name}: {} new document(s) ({} total).",
    group_thousands(after - before),
    group_thousands(after)
  );
}

/// Activates a `service` on a `corpus` synchronously — the CLI surface of the corpus screen's
/// "Register a service" form and the agent `POST /api/corpora/<c>/services/<s>`, driving the same
/// `Backend::register_service` (which is idempotent-neutral: it *refuses* an already-activated pair
/// rather than wiping its results). Creates one TODO task per imported document, then prints the
/// count. Exits `1` on an unknown corpus/service, an infrastructure service, or an activation
/// error.
fn run_activate(corpus_name: String, service_name: String, owner: String, description: String) {
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
  // Magic ids 1=init, 2=import are infrastructure (CLAUDE.md); activating them is nonsensical and
  // the backend would otherwise reject it as "already registered" — give a clear reason instead.
  if service.id <= 2 {
    eprintln!(
      "'{}' is an infrastructure service (init/import) and cannot be activated on a corpus.",
      service.name
    );
    std::process::exit(1);
  }
  println!("Activating {} on {} …", service.name, corpus.name);
  // `register_service` looks the corpus up by path and creates a TODO task per imported document.
  // It is idempotent-neutral: an already-activated pair returns a descriptive Err (no wipe).
  if let Err(error) = backend.register_service(&service, &corpus.path, owner, description) {
    eprintln!("Activation failed: {error}");
    std::process::exit(1);
  }
  // Each activated pair gets one task per imported document, so the corpus's document count is the
  // number just queued.
  let queued = Corpus::document_counts(&mut backend.connection)
    .get(&corpus.id)
    .copied()
    .unwrap_or(0);
  println!(
    "Activated {} on {} — {} task(s) queued. Start the dispatcher to convert them.",
    service.name,
    corpus.name,
    group_thousands(queued)
  );
}

/// Deactivates (retires) a `service` from a `corpus` — the CLI surface of the corpus screen's
/// per-service deactivate form and the agent `DELETE /api/corpora/<c>/services/<s>`, via the
/// transactional, orphan-free `Service::deactivate_from_corpus`. The inverse of `activate`. Dry-run
/// by default (prints the pair's task count); `--yes` executes. Refuses the infrastructure
/// init/import services. Exits `1` on an unknown corpus/service or a deactivation error.
fn run_deactivate(corpus_name: String, service_name: String, yes: bool) {
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
  // Deactivating `import` (2) would wipe the corpus's document registry; `init` (1) is
  // infrastructure too. The web screen never offers these — refuse them here as well.
  if service.id <= 2 {
    eprintln!(
      "'{}' is an infrastructure service (init/import) and cannot be deactivated from a corpus.",
      service.name
    );
    std::process::exit(1);
  }
  let task_count = service
    .task_count_for_corpus(&corpus, &mut backend.connection)
    .unwrap_or(-1);

  if !yes {
    println!(
      "Dry run — would deactivate '{}' from '{}', deleting its tasks + log messages for this corpus:",
      service.name, corpus.name
    );
    println!(
      "  {} task(s) (the service definition + its work on other corpora survive; run tallies are immutable).",
      group_thousands(task_count)
    );
    println!("Pass --yes to deactivate.");
    return;
  }
  match service.deactivate_from_corpus(&corpus, &mut backend.connection) {
    Ok(removed) => println!(
      "Deactivated '{}' from '{}' ({} task(s) removed).",
      service.name,
      corpus.name,
      group_thousands(removed as i64)
    ),
    Err(error) => {
      eprintln!("Deactivation failed: {error}");
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
  max_archive_mb: Option<u64>,
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
    max_archive_mb,
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

fn run_revoke_token(token: Option<String>, owner: Option<String>) {
  match (token.as_deref(), owner.as_deref()) {
    (None, None) => {
      eprintln!(
        "error: provide a <TOKEN> to revoke, or --owner <name> to revoke all of an owner's tokens"
      );
      std::process::exit(2);
    },
    (Some(_), Some(_)) => {
      eprintln!("error: provide EITHER a <TOKEN> or --owner <name>, not both");
      std::process::exit(2);
    },
    _ => {},
  }
  match bootstrap::revoke_admin_token(&config_file_path(), token.as_deref(), owner.as_deref()) {
    Ok(outcome) => {
      if outcome.revoked == 0 {
        println!(
          "No matching token found — nothing revoked ({} token(s) still configured).",
          outcome.token_count
        );
      } else {
        println!(
          "Revoked {} token(s) in {} ({} remaining).",
          outcome.revoked,
          config_file_path().display(),
          outcome.token_count
        );
      }
      if outcome.shadowed_by_legacy_json {
        eprintln!(
          "\nWARNING: a legacy config.json in this directory overrides [auth] in cortex.toml, so \
           this revoke will NOT take effect until you remove the token from config.json (or remove \
           config.json)."
        );
      }
    },
    Err(error) => {
      eprintln!("cortex revoke-token failed: {error}");
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
      // Bridge a fresh install to the first conversion run: the doctor nudge above covers the admin
      // token; this points at the end-to-end walkthrough (corpus → service → register → run) so the
      // operator knows what comes after `init`.
      println!(
        "\nThen stand up your first corpus + service and run a conversion — the end-to-end \
         quickstart is the first tutorial in MANUAL.md (`cortex <command> --help` documents any step)."
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

/// `cortex status` — the operational snapshot (the CLI twin of the admin dashboard's live-ops
/// console). Reuses the same library queries the dashboard and `/metrics` run, so all three
/// surfaces agree. `--json` mirrors the `/admin/status.json` `AdminStatusDto` shape (minus the
/// server-only connection-pool counters). Never prints the DB URL (it carries the password).
fn run_status(json: bool) {
  let mut backend = backend::from_address(default_db_address());
  let connection = &mut backend.connection;

  let corpus_count = Corpus::all(connection).map_or(0, |corpora| corpora.len());
  let active_sessions = Session::active(connection).map_or(0, |sessions| sessions.len());
  let active_jobs = cortex::jobs::list_recent(connection, true, 200).len();
  let jobs_failed_recent = cortex::jobs::count_recent_with_status(connection, "failed", 24);
  let (workers_total, workers_in_flight) =
    WorkerMetadata::fleet_summary(connection).unwrap_or((0, 0));
  let tasks_todo = Task::count_todo(connection);
  let last_run = HistoricalRun::recent_all(connection, 1)
    .ok()
    .and_then(|runs| runs.into_iter().next())
    .map(|run| run.with_live_tallies(connection));

  if json {
    let last = last_run.as_ref().map(|run| {
      serde_json::json!({
        "when": iso(run.start_time),
        "owner": run.owner,
        "description": run.description,
        "total": run.total,
        "in_progress": run.in_progress,
        "open": run.end_time.is_none(),
      })
    });
    let status = serde_json::json!({
      "corpus_count": corpus_count,
      "tasks_todo": tasks_todo,
      "workers_total": workers_total,
      "workers_in_flight": workers_in_flight,
      "active_sessions": active_sessions,
      "active_jobs": active_jobs,
      "jobs_failed_recent": jobs_failed_recent,
      "last_run": last,
    });
    println!(
      "{}",
      serde_json::to_string_pretty(&status).unwrap_or_default()
    );
  } else {
    println!("CorTeX status:");
    println!("  corpora:          {corpus_count}");
    println!(
      "  pending tasks:    {}   (awaiting conversion / TODO)",
      group_thousands(tasks_todo)
    );
    // The *active* fleet (workers that dispatched/returned within ~10 min) + their in-flight tasks
    // — a truthful "what's running now" signal that reads 0 on an idle deployment, rather than
    // the old lifetime gap (KNOWN_ISSUES P-3); matches the dashboard/`/metrics` for
    // cross-checking.
    println!(
      "  workers:          {workers_total} active · {} in-flight",
      group_thousands(workers_in_flight)
    );
    println!("  background jobs:  {active_jobs} active · {jobs_failed_recent} failed (24h)");
    println!("  admin sessions:   {active_sessions} active");
    match &last_run {
      Some(run) => println!(
        "  last run:         {}  by {}  —  {}  ({} tasks{})",
        iso(run.start_time),
        run.owner,
        run.description,
        run.total,
        if run.end_time.is_none() { ", open" } else { "" }
      ),
      None => println!("  last run:         none yet"),
    }
  }
}

/// Lists recent background jobs — the CLI surface of the `/jobs` dashboard and the agent
/// `GET /api/jobs`, sharing the same `jobs::list_recent` + `JobDto`. `--active` narrows to
/// pending/running; `--json` mirrors the agent `JobDto` list. The text view shows each job's
/// health, progress, runtime, and heartbeat-idle age (a large idle on a running job flags a stall).
fn run_jobs(active: bool, limit: Option<i64>, json: bool) {
  let mut backend = backend::from_address(default_db_address());
  let limit = limit.unwrap_or(50).clamp(1, 200);
  let now = cortex::jobs::db_now(&mut backend.connection);
  let dtos: Vec<JobDto> = cortex::jobs::list_recent(&mut backend.connection, active, limit)
    .into_iter()
    .map(|job| JobDto::at(job, now))
    .collect();
  if json {
    println!(
      "{}",
      serde_json::to_string_pretty(&dtos).unwrap_or_default()
    );
    return;
  }
  if dtos.is_empty() {
    println!("No {}background jobs.", if active { "active " } else { "" });
    return;
  }
  println!("{} background job(s):", dtos.len());
  for job in &dtos {
    let progress = match job.progress_total {
      Some(total) => format!(
        "{}/{}",
        group_thousands(i64::from(job.progress_current)),
        group_thousands(i64::from(total))
      ),
      None => group_thousands(i64::from(job.progress_current)),
    };
    // Flag a stalled running job: a large heartbeat-idle age while non-terminal.
    let idle =
      if (job.health == "running" || job.health == "pending") && job.seconds_since_update >= 120 {
        format!("{}s ⚠", job.seconds_since_update)
      } else {
        format!("{}s", job.seconds_since_update)
      };
    println!(
      "  [{}] {}  by {}  ·  {} done  ·  {}s runtime  ·  idle {}",
      job.health, job.kind, job.actor, progress, job.duration_seconds, idle
    );
    if !job.message.trim().is_empty() {
      println!("        {}", job.message.trim());
    }
  }
}

/// Reviews the accountability audit log — the CLI surface of the `/admin/audit` screen and the
/// agent `GET /api/audit`, sharing `AuditEntry::list` + the `AuditDto`. Lists the most-recent admin
/// actions (who / what / outcome / when), optionally filtered by `--actor`; `--json` mirrors the
/// agent list.
fn run_audit(actor: Option<String>, limit: Option<i64>, json: bool) {
  let mut backend = backend::from_address(default_db_address());
  let limit = limit.unwrap_or(50).clamp(1, 500);
  let entries = match AuditEntry::list(&mut backend.connection, actor.as_deref(), limit, 0) {
    Ok(entries) => entries,
    Err(error) => {
      eprintln!("Could not read the audit log: {error}");
      std::process::exit(1);
    },
  };
  let dtos: Vec<AuditDto> = entries.into_iter().map(AuditDto::from).collect();
  if json {
    println!(
      "{}",
      serde_json::to_string_pretty(&dtos).unwrap_or_default()
    );
    return;
  }
  if dtos.is_empty() {
    println!(
      "No audit entries{}.",
      actor
        .map(|a| format!(" for actor {a:?}"))
        .unwrap_or_default()
    );
    return;
  }
  println!("{} audit entr(y/ies), most recent first:", dtos.len());
  for entry in &dtos {
    println!(
      "  {}  {}  {} {}  →  {}",
      entry.at, entry.actor, entry.action, entry.target, entry.outcome
    );
    if !entry.details.trim().is_empty() {
      println!("        {}", entry.details.trim());
    }
  }
}

/// Lists all registered corpora — the CLI surface of the overview screen and the agent
/// `GET /api/corpora`, sharing `Corpus::all` + `Corpus::document_counts` + the `CorpusDto`. The
/// text view leads with each corpus's `public_id` (the stable external handle), then name · doc
/// count · path — the handles/names the other subcommands take as input; `--json` mirrors the agent
/// list.
fn run_corpora(json: bool) {
  let mut backend = backend::from_address(default_db_address());
  let connection = &mut backend.connection;
  let counts = Corpus::document_counts(connection);
  let all = Corpus::all(connection).unwrap_or_default();
  // id → name over the loaded listing, so a sandbox's parent name resolves with no extra query.
  let names_by_id: std::collections::HashMap<i32, String> =
    all.iter().map(|c| (c.id, c.name.clone())).collect();
  let dtos: Vec<CorpusDto> = all
    .into_iter()
    .map(|corpus| {
      let count = counts.get(&corpus.id).copied().unwrap_or(0);
      let parent = corpus
        .parent_corpus_id
        .and_then(|pid| names_by_id.get(&pid).cloned());
      CorpusDto::build(corpus, count, parent)
    })
    .collect();
  if json {
    println!(
      "{}",
      serde_json::to_string_pretty(&dtos).unwrap_or_default()
    );
    return;
  }
  if dtos.is_empty() {
    println!("No corpora registered. Import one with `cortex import <name> <path>`.");
    return;
  }
  println!("{} corpus(es):", dtos.len());
  for corpus in &dtos {
    let sandbox = match &corpus.parent {
      Some(parent) => format!("  ·  sandbox of {parent}"),
      None => String::new(),
    };
    println!(
      "  {}  {}  ·  {} docs{}  ·  {}{}",
      corpus.public_id,
      corpus.name,
      group_thousands(corpus.document_count),
      if corpus.complex { " (complex)" } else { "" },
      corpus.path,
      sandbox
    );
  }
}

/// Lists the service registry — the CLI surface of the registry screen and the agent
/// `GET /api/services`, sharing `Service::all` + the `ServiceDto`. The text view leads with each
/// service's `public_id`, then name · version · input→output (incl. the magic init/import
/// services); `--json` mirrors the agent list.
fn run_services(json: bool) {
  let mut backend = backend::from_address(default_db_address());
  let dtos: Vec<ServiceDto> = Service::all(&mut backend.connection)
    .unwrap_or_default()
    .into_iter()
    .map(ServiceDto::from)
    .collect();
  if json {
    println!(
      "{}",
      serde_json::to_string_pretty(&dtos).unwrap_or_default()
    );
    return;
  }
  if dtos.is_empty() {
    println!("No services registered.");
    return;
  }
  println!("{} service(s):", dtos.len());
  for service in &dtos {
    println!(
      "  {}  {}  v{}  ·  {} → {}{}",
      service.public_id,
      service.name,
      service.version,
      service.inputformat,
      service.outputformat,
      if service.complex { " (complex)" } else { "" }
    );
  }
}
