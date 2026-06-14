// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Historical-data **retention** — managing the one unbounded-growth table, `historical_tasks` (one
//! per-task status snapshot per save-snapshot). The admin screen surfaces the snapshot count + the
//! oldest snapshot, lets the admin pick a cutoff date, shows a **dry-run count** of exactly what a
//! prune would remove, and only then offers a confirmed delete (gated + audited — same safety
//! pattern as `delete_corpus`). The run *summaries* (`historical_runs`) are never touched, so the
//! run history and charts survive; only the bulky per-task snapshots age out.

use chrono::{NaiveDate, NaiveDateTime};
use rocket::form::Form;
use rocket::http::Status;
use rocket::response::Redirect;
use rocket::serde::json::Json;
use rocket::{Route, State};
use rocket_dyn_templates::{context, Template};
use serde::Serialize;

use crate::backend::DbPool;
use crate::frontend::actor::{require_admin_to, Actor, AdminReject, AdminSession, ReturnTo};
use crate::models::HistoricalTask;

/// Parses a `YYYY-MM-DD` cutoff to the start of that day (midnight); `None` on a malformed date.
fn parse_cutoff(date: &str) -> Option<NaiveDateTime> {
  NaiveDate::parse_from_str(date, "%Y-%m-%d")
    .ok()?
    .and_hms_opt(0, 0, 0)
}

/// Formats an optional snapshot timestamp for display.
fn fmt_oldest(oldest: Option<NaiveDateTime>) -> String {
  oldest.map_or_else(
    || "none".to_string(),
    |when| when.format("%Y-%m-%d %H:%M").to_string(),
  )
}

/// Per-task snapshot retention stats, as exposed over the API/UI.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct HistoricalStatsDto {
  /// Total per-task snapshot rows (`historical_tasks`) — the unbounded-growth table.
  pub snapshot_rows: i64,
  /// The oldest snapshot's timestamp, formatted (`none` if there are no snapshots).
  pub oldest: String,
}

/// The per-task snapshot retention stats (agent twin of the `/admin/retention` screen). **Token-
/// gated.** `503` if the pool is exhausted.
#[rocket_okapi::openapi(tag = "Management")]
#[get("/api/historical/stats")]
pub fn api_historical_stats(
  _caller: Actor,
  pool: &State<DbPool>,
) -> Result<Json<HistoricalStatsDto>, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let (total, oldest) =
    HistoricalTask::retention_stats(&mut connection).map_err(|_| Status::InternalServerError)?;
  Ok(Json(HistoricalStatsDto {
    snapshot_rows: total,
    oldest: fmt_oldest(oldest),
  }))
}

/// The data-retention screen (`GET /admin/retention?<before>&<pruned>`): snapshot stats; with
/// `?before=YYYY-MM-DD`, a **dry-run** count of how many snapshots that cutoff would prune (the
/// page then offers a confirmed delete). `?pruned=N` flashes the result of a completed prune.
/// Signed-in admins only (unauthenticated → sign-in, returning here).
#[allow(clippy::result_large_err)] // AdminReject carries a Redirect; see actor::AdminReject.
#[get("/admin/retention?<before>&<pruned>")]
pub fn retention_page(
  before: Option<String>,
  pruned: Option<i64>,
  session: Option<AdminSession>,
  return_to: ReturnTo,
  pool: &State<DbPool>,
) -> Result<Template, AdminReject> {
  let admin = require_admin_to(session, &return_to)?;
  let mut snapshot_rows = 0i64;
  let mut oldest = "none".to_string();
  let mut preview: Option<serde_json::Value> = None;
  if let Ok(mut connection) = pool.get() {
    if let Ok((total, old)) = HistoricalTask::retention_stats(&mut connection) {
      snapshot_rows = total;
      oldest = fmt_oldest(old);
    }
    // A dry-run preview only when the cutoff parses (a bad date shows no preview, never deletes).
    if let Some(cutoff) = before.as_deref().and_then(parse_cutoff) {
      let count = HistoricalTask::count_before(&mut connection, cutoff).unwrap_or(0);
      preview = Some(serde_json::json!({ "before": before, "count": count }));
    }
  }
  let global = serde_json::json!({
    "title": "Historical data retention",
    "description": "Prune old per-task status snapshots",
  });
  Ok(Template::render(
    "retention",
    context! { global, owner: admin.owner, snapshot_rows, oldest, preview, pruned },
  ))
}

/// The prune form: the cutoff date the preview was shown for.
#[derive(FromForm)]
pub struct PruneForm {
  /// `YYYY-MM-DD` — snapshots strictly older than midnight of this day are pruned.
  pub before: String,
}

/// Prunes per-task snapshots older than the cutoff (`POST /admin/retention/prune`). Confirmed by
/// the two-step preview + the form's `confirm()` dialog; **gated** (AdminSession) and **audited**
/// (the audit fairing records the action + actor). Only `historical_tasks` is touched — run
/// summaries survive. Redirects back to the screen with the count removed; a malformed cutoff is a
/// no-op.
#[allow(clippy::result_large_err)] // AdminReject carries a Redirect; see actor::AdminReject.
#[post("/admin/retention/prune", data = "<form>")]
pub fn prune(
  form: Form<PruneForm>,
  session: Option<AdminSession>,
  return_to: ReturnTo,
  pool: &State<DbPool>,
) -> Result<Redirect, AdminReject> {
  let admin = require_admin_to(session, &return_to)?;
  let Some(cutoff) = parse_cutoff(&form.before) else {
    return Ok(Redirect::to("/admin/retention"));
  };
  let pruned = pool
    .get()
    .ok()
    .and_then(|mut connection| HistoricalTask::prune_before(&mut connection, cutoff).ok())
    .unwrap_or(0);
  // Server-side record of who pruned what (the audit fairing also logs the action + actor +
  // outcome).
  println!(
    "-- retention: {:?} pruned {pruned} historical_tasks older than {}",
    admin.owner, form.before
  );
  Ok(Redirect::to(format!("/admin/retention?pruned={pruned}")))
}

/// The human retention screen + prune (the agent `api_historical_stats` is mounted via
/// `frontend::apidoc`).
pub fn routes() -> Vec<Route> { routes![retention_page, prune] }
