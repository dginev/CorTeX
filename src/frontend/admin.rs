// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Admin web UI: a single **signed-in** `/admin` dashboard that consolidates the admin actions
//! (service registry, background jobs, system health, settings, API docs, and "add a corpus") which
//! previously sprinkled the public homepage. Access uses the lightweight token scheme — an
//! [`AdminSession`] cookie (`frontend::actor`), set on the sign-in page below.

use diesel::sql_types::{BigInt, Integer, Nullable, Text};
use diesel::{PgConnection, QueryableByName, RunQueryDsl, sql_query};
use rocket::form::Form;
use rocket::http::{Cookie, CookieJar, SameSite, Status};
use rocket::response::Redirect;
use rocket::serde::json::Json;
use rocket::{Route, State};
use rocket_dyn_templates::{Template, context};
use schemars::JsonSchema;
use serde::Serialize;

use crate::backend::DbPool;
use crate::frontend::actor::{
  ADMIN_COOKIE, Actor, AdminSession, ReturnTo, owner_for_token, safe_next, sign_in_url,
};
use crate::models::{Corpus, HistoricalRun, Session, Task, WorkerMetadata};

/// At-a-glance operational snapshot for the admin **live ops console** — the small-table signals
/// (plus the one pending-task backlog count) the dashboard polls every few seconds and renders
/// server-side on first paint. Every field is best-effort: a database hiccup degrades it to
/// `0`/`None` rather than failing the screen. Deliberately excludes the dispatcher-port /
/// corpus-storage probes (those are the System Health screen's job — and too slow to poll). Agents
/// get this same snapshot from the token-gated `GET /api/status` ([`api_status`]) or the Prometheus
/// gauges at `/metrics`.
#[derive(Serialize, JsonSchema)]
pub struct AdminStatusDto {
  /// Registered corpora.
  pub corpus_count: usize,
  /// Background jobs currently queued or running.
  pub active_jobs: usize,
  /// Active (unexpired) admin sessions.
  pub active_sessions: usize,
  /// Workers **active in the last ~10 minutes** (dispatched or returned a task) — the
  /// actively-converting fleet, not all registered rows. `0` when no dispatcher is running.
  pub workers_total: i64,
  /// Tasks in-flight (dispatched, not yet returned) at those **active** workers — real current
  /// in-flight work, `0` on an idle deployment (no longer a cumulative-lifetime tally;
  /// KNOWN_ISSUES P-3).
  pub workers_in_flight: i64,
  /// Tasks awaiting conversion (status TODO, not yet dispatched) — the pending-work backlog, the
  /// human twin of the `cortex_tasks_todo` `/metrics` gauge.
  pub tasks_todo: i64,
  /// Background jobs that ended `failed` within the last 24h (rolling window).
  pub jobs_failed_recent: usize,
  /// Pooled connections currently checked out (saturation signal).
  pub pool_in_use: u32,
  /// Maximum size of the frontend connection pool.
  pub pool_max: u32,
  /// The most recent conversion run (live tallies overlaid while it is still open), if any.
  pub last_run: Option<LastRunDto>,
}

/// The latest run's headline, with live task tallies overlaid while it is still open (so the card
/// shows real progress, not a frozen-at-completion zero).
#[derive(Serialize, JsonSchema)]
pub struct LastRunDto {
  /// Run start time, ISO-8601 UTC.
  pub when: String,
  /// The actor who launched the run.
  pub owner: String,
  /// The run's description.
  pub description: String,
  /// Total tasks in the run.
  pub total: i32,
  /// Tasks still in progress (live).
  pub in_progress: i32,
  /// Whether the run is still open (tallies not yet frozen).
  pub open: bool,
}

/// Gathers the [`AdminStatusDto`] over one pooled connection (plus the in-memory pool counters).
/// Shared by the HTML dashboard's first paint and the `/admin/status.json` poll feed so both show
/// identical state. Mostly cheap small-table reads (plus the one `tasks_todo` backlog count) — no
/// dispatcher/storage probe.
pub fn admin_status(pool: &DbPool) -> AdminStatusDto {
  // Pool counters are in-memory — available even if the database is unreachable.
  let state = pool.state();
  let mut status = AdminStatusDto {
    corpus_count: 0,
    active_jobs: 0,
    active_sessions: 0,
    workers_total: 0,
    workers_in_flight: 0,
    tasks_todo: 0,
    jobs_failed_recent: 0,
    pool_in_use: state.connections.saturating_sub(state.idle_connections),
    pool_max: pool.max_size(),
    last_run: None,
  };
  if let Ok(mut connection) = pool.get() {
    status.corpus_count = Corpus::all(&mut connection).map_or(0, |corpora| corpora.len());
    status.active_jobs = crate::jobs::list_recent(&mut connection, true, 200).len();
    status.active_sessions = Session::active(&mut connection).map_or(0, |sessions| sessions.len());
    status.jobs_failed_recent =
      crate::jobs::count_recent_with_status(&mut connection, "failed", 24);
    if let Ok((workers, in_flight)) = WorkerMetadata::fleet_summary(&mut connection) {
      status.workers_total = workers;
      status.workers_in_flight = in_flight;
    }
    // Pending-conversion backlog (the unleased work waiting for the fleet) — the one full-table
    // count here, degrading to 0 on error like its siblings.
    status.tasks_todo = Task::count_todo(&mut connection);
    status.last_run = HistoricalRun::recent_all(&mut connection, 1)
      .ok()
      .and_then(|runs| runs.into_iter().next())
      .map(|run| {
        // The latest run is often still open (tallies frozen only at completion) — overlay live
        // progress so the card shows real task counts, not a misleading zero.
        let run = run.with_live_tallies(&mut connection);
        LastRunDto {
          when: crate::frontend::helpers::iso_utc(run.start_time),
          owner: run.owner,
          description: run.description,
          total: run.total,
          in_progress: run.in_progress,
          open: run.end_time.is_none(),
        }
      });
  }
  status
}

/// The admin dashboard (`GET /admin`): the consolidated home for admin actions. **Signed-in admins
/// only** — an unauthenticated browser is redirected to the sign-in page (`Err(Redirect)`).
// `Redirect` (Rocket's URI responder) is a chunky type, so the `Err` variant trips
// `result_large_err` — irrelevant for a one-shot request handler; the page-or-redirect `Result` is
// the idiomatic shape.
#[allow(clippy::result_large_err)]
#[get("/admin")]
pub fn admin_page(
  session: Option<AdminSession>,
  return_to: ReturnTo,
  pool: &State<DbPool>,
) -> Result<Template, Redirect> {
  let session = session.ok_or_else(|| Redirect::to(sign_in_url(false, Some(&return_to.0))))?;
  // The command center's first paint — the same snapshot the page then polls live from
  // `/admin/status.json` (one shared DTO, so server-render and live update never diverge).
  let status = admin_status(pool);
  let global = serde_json::json!({
    "title": "Admin",
    "description": "CorTeX administration dashboard",
  });
  Ok(Template::render(
    "admin",
    context! { global, owner: session.owner, status },
  ))
}

/// `GET /admin/status.json` — the live ops console's poll feed: the [`AdminStatusDto`] as JSON, for
/// the dashboard's few-second auto-refresh. **Cookie-gated** (a signed-in [`AdminSession`]); an
/// expired session returns `401` so the page simply keeps its last-good values rather than
/// redirecting an XHR. Same-origin only — the agent twin (token-gated, same DTO) is
/// [`api_status`] (`GET /api/status`); the Prometheus gauges are at `/metrics`.
#[get("/admin/status.json")]
pub fn admin_status_feed(
  session: Option<AdminSession>,
  pool: &State<DbPool>,
) -> Result<Json<AdminStatusDto>, Status> {
  let _session = session.ok_or(Status::Unauthorized)?;
  Ok(Json(admin_status(pool)))
}

/// `GET /api/status` — the **agent twin** of the dashboard's `/admin/status.json` feed: the
/// [`AdminStatusDto`] system snapshot (corpus count, the worker fleet, background-job activity, the
/// pending-conversion backlog, and the latest run) as one structured JSON call a monitoring agent
/// can poll. Complements the Prometheus `/metrics` gauges — it carries the structured `last_run`
/// detail (owner / description / timing) the gauges can't, and matches `cortex status --json`.
/// **Token-gated** via the [`Actor`] guard (`401` without a valid token).
#[rocket_okapi::openapi(tag = "Management")]
#[get("/api/status")]
pub fn api_status(_actor: Actor, pool: &State<DbPool>) -> Json<AdminStatusDto> {
  Json(admin_status(pool))
}

/// One worker's live activity row for the [`LiveActivityDto`] fleet feed — the "what the fleet is
/// doing now" signal, read straight from the `worker_metadata` rows the dispatcher already keeps.
#[derive(Serialize, JsonSchema)]
pub struct FleetWorkerDto {
  /// Worker identity (usually `hostname:pid`).
  pub name: String,
  /// The service this worker serves.
  pub service_id: i32,
  /// Lifetime results this worker has returned.
  pub total_returned: i32,
  /// The most recent task id this worker returned a result for (`None` if it never has).
  pub last_returned_task_id: Option<i64>,
  /// When this worker was last dispatched a task (RFC 3339 UTC).
  pub time_last_dispatch: String,
  /// When this worker last returned a result (RFC 3339 UTC), if ever.
  pub time_last_return: Option<String>,
}

/// One recent conversion message (fatal/error/warning) for the [`LiveActivityDto`] stream — read
/// from the `log_*` rows the dispatcher's finalize thread already persists, joined to the task's
/// entry/corpus/service. The live signal of a run's health.
#[derive(Serialize, JsonSchema)]
pub struct ActivityMessageDto {
  /// `"fatal"`, `"error"`, or `"warning"`.
  pub severity: String,
  /// The corpus the converting task belongs to.
  pub corpus: String,
  /// The service that produced the message.
  pub service: String,
  /// The document entry (source path) that errored.
  pub entry: String,
  /// The message category (e.g. `undefined`), if any.
  pub category: Option<String>,
  /// The message subject (`what`), if any.
  pub what: Option<String>,
  /// The message detail, truncated for the feed.
  pub details: Option<String>,
  /// When the message was recorded (RFC 3339 UTC); `None` for rows written before the
  /// `recorded_at` column existed (they sort last in the stream).
  pub when: Option<String>,
}

/// The admin **live activity** feed: the actively-converting fleet plus the most recent conversion
/// problems. Every field is read-only over data the dispatcher already writes to Postgres as its
/// normal work — the frontend polls it and the **dispatcher is never in the request loop**, so a
/// slow or absent UI can never back-pressure or endanger the conversion hot path.
#[derive(Serialize, JsonSchema)]
pub struct LiveActivityDto {
  /// Recently-active workers, newest dispatch first.
  pub fleet: Vec<FleetWorkerDto>,
  /// The most recent conversion messages — fatals, errors, and warnings **intermeshed and sorted
  /// newest-first by `recorded_at`** (a live streaming-log tail), capped at ~100.
  pub recent: Vec<ActivityMessageDto>,
}

/// A raw recent-message row from the unified [`recent_activity`] UNION — already carries its
/// `severity` (a SQL literal per branch) and the `recorded_at` timestamp formatted to ISO in SQL
/// (`when_iso`), so ordering is a lexical sort (ISO 8601 sorts chronologically) with no chrono type
/// to thread through the projection.
#[derive(QueryableByName)]
struct MessageRow {
  #[diesel(sql_type = Text)]
  severity: String,
  #[diesel(sql_type = Text)]
  entry: String,
  #[diesel(sql_type = Text)]
  corpus: String,
  #[diesel(sql_type = Text)]
  service: String,
  #[diesel(sql_type = Nullable<Text>)]
  category: Option<String>,
  #[diesel(sql_type = Nullable<Text>)]
  what: Option<String>,
  #[diesel(sql_type = Nullable<Text>)]
  details: Option<String>,
  #[diesel(sql_type = Nullable<Text>)]
  when_iso: Option<String>,
}

impl MessageRow {
  fn into_dto(self) -> ActivityMessageDto {
    // Cap detail length so the live-poll payload stays small (details can be up to 2000 chars).
    let details = self.details.map(|d| {
      const MAX: usize = 300;
      if d.chars().count() <= MAX {
        d
      } else {
        d.chars().take(MAX).collect::<String>() + "…"
      }
    });
    ActivityMessageDto {
      severity: self.severity,
      corpus: self.corpus,
      service: self.service,
      entry: self.entry,
      category: self.category,
      what: self.what,
      details,
      when: self.when_iso,
    }
  }
}

/// The most recent fatal/error/warning messages for one **service**, **intermeshed and sorted
/// newest-first by `recorded_at`** — a unified streaming-log tail. Each severity-partitioned table
/// has its own BIGSERIAL `id` sequence (not comparable across tables), so the shared `recorded_at`
/// timestamp is the only valid cross-severity ordering key; it is formatted to ISO in SQL so the
/// outer `ORDER BY` is a lexical (= chronological) sort with `NULLS LAST` for pre-migration rows.
/// Each inner branch is bounded by `per_table` via its own `id`-PK scan (index-cheap even on the
/// ~100M-row production tables); the outer `LIMIT total` takes the global newest. The table names +
/// severity literals are fixed (never user input). Best-effort: a query error degrades to empty.
fn recent_activity(
  connection: &mut PgConnection,
  service_id: i32,
  per_table: i64,
  total: i64,
) -> Vec<MessageRow> {
  // One UNION branch per severity table; `$1` = service_id, `$2` = per-table cap, `$3` = total cap.
  let branch = |table: &str, severity: &str| {
    format!(
      "(SELECT '{severity}' AS severity, t.entry AS entry, c.name AS corpus, s.name AS service, \
               l.category AS category, l.what AS what, l.details AS details, \
               to_char(l.recorded_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS when_iso \
        FROM {table} l \
        JOIN tasks t ON t.id = l.task_id \
        JOIN corpora c ON c.id = t.corpus_id \
        JOIN services s ON s.id = t.service_id \
        WHERE t.service_id = $1 \
        ORDER BY l.id DESC LIMIT $2)"
    )
  };
  let query = format!(
    "SELECT severity, entry, corpus, service, category, what, details, when_iso FROM ( {} UNION ALL \
     {} UNION ALL {} ) u ORDER BY when_iso DESC NULLS LAST LIMIT $3",
    branch("log_fatals", "fatal"),
    branch("log_errors", "error"),
    branch("log_warnings", "warning"),
  );
  sql_query(query)
    .bind::<Integer, _>(service_id)
    .bind::<BigInt, _>(per_table)
    .bind::<BigInt, _>(total)
    .get_results::<MessageRow>(connection)
    .unwrap_or_default()
}

/// Gathers the [`LiveActivityDto`] over one pooled connection. Like [`admin_status`], every read is
/// best-effort (degrades to an empty list) and **read-only** — the dispatcher is never involved, so
/// the live feed cannot perturb the conversion hot path.
///
/// The feed is **conditioned on the active run's service** — the one actually generating messages
/// now (the latest [`HistoricalRun`]). A co-resident legacy service (e.g. a Perl `tex-to-html`
/// sharing the DB with the Rust `oxidized-tex-to-html` run) therefore never pollutes the live view;
/// the feed follows whatever is converting. With no runs yet, the feed is simply empty.
pub fn live_activity(pool: &DbPool, limit: i64) -> LiveActivityDto {
  let mut activity = LiveActivityDto {
    fleet: Vec::new(),
    recent: Vec::new(),
  };
  if let Ok(mut connection) = pool.get() {
    let active_service = HistoricalRun::recent_all(&mut connection, 1)
      .ok()
      .and_then(|runs| runs.into_iter().next())
      .map(|run| run.service_id);
    let Some(service_id) = active_service else {
      return activity;
    };
    if let Ok(workers) = WorkerMetadata::recent(&mut connection, 200) {
      activity.fleet = workers
        .into_iter()
        .filter(|w| w.service_id == service_id)
        .take(80)
        .map(|w| FleetWorkerDto {
          name: w.name,
          service_id: w.service_id,
          total_returned: w.total_returned,
          last_returned_task_id: w.last_returned_task_id,
          time_last_dispatch: crate::models::iso_utc_system(w.time_last_dispatch),
          time_last_return: w.time_last_return.map(crate::models::iso_utc_system),
        })
        .collect();
    }
    // Fetch up to `limit` per severity, intermeshed + sorted newest-first by recorded_at, capped at
    // `limit` total — a unified streaming-log tail.
    activity.recent = recent_activity(&mut connection, service_id, limit, limit)
      .into_iter()
      .map(MessageRow::into_dto)
      .collect();
  }
  activity
}

/// `GET /admin/logs.json` — the live-activity panel's poll feed: the [`LiveActivityDto`] as JSON.
/// **Cookie-gated** (a signed-in [`AdminSession`]); an expired session returns `401` so the page
/// keeps its last-good values. The agent twin is [`api_logs`] (`GET /api/logs`).
#[get("/admin/logs.json")]
pub fn admin_logs_feed(
  session: Option<AdminSession>,
  pool: &State<DbPool>,
) -> Result<Json<LiveActivityDto>, Status> {
  let _session = session.ok_or(Status::Unauthorized)?;
  Ok(Json(live_activity(pool, 100)))
}

/// `GET /api/logs` — the **agent twin** of the dashboard's `/admin/logs.json` feed: the live fleet
/// activity plus the most recent fatal/error conversion messages as one structured JSON call a
/// monitoring agent can poll. **Token-gated** via the [`Actor`] guard.
#[rocket_okapi::openapi(tag = "Management")]
#[get("/api/logs")]
pub fn api_logs(_actor: Actor, pool: &State<DbPool>) -> Json<LiveActivityDto> {
  Json(live_activity(pool, 100))
}

/// The sign-in page (`GET /admin/login?<bad>&<next>`): a form to enter an admin token, plus a "sign
/// in with a passkey" affordance when passkeys are enabled. `?bad=true` flags a failed previous
/// attempt; `?next=` is the destination to return to after signing in (carried through the form).
#[get("/admin/login?<bad>&<next>")]
pub fn admin_login_page(
  bad: Option<bool>,
  next: Option<String>,
  webauthn: &State<Option<crate::frontend::webauthn::WebauthnState>>,
) -> Template {
  let global = serde_json::json!({
    "title": "Admin sign-in",
    "description": "Sign in to the CorTeX admin dashboard",
  });
  // Only carry a safe local `next` into the page (open-redirect guard; also avoids reflecting
  // junk).
  let next = next.filter(|path| path.starts_with('/') && !path.starts_with("//"));
  Template::render(
    "admin-login",
    context! { global, bad: bad.unwrap_or(false), next, passkeys_enabled: webauthn.inner().is_some() },
  )
}

/// The sign-in form fields.
#[derive(FromForm)]
pub struct LoginForm {
  /// A rerun token (resolved to an owner via `auth.rerun_tokens`).
  pub token: String,
  /// Where to return after a successful sign-in (validated to a safe local path).
  pub next: Option<String>,
}

/// Processes sign-in (`POST /admin/login`): validates the token against `auth.rerun_tokens`; on
/// success **opens a server-side session** and sets the [`ADMIN_COOKIE`] cookie to its random
/// opaque id (HttpOnly, SameSite=Lax) — the cookie no longer carries the token — then redirects to
/// the validated `next` destination (default `/admin`). A bad token (or a DB hiccup opening the
/// session) returns to the sign-in page flagged, preserving `next`.
#[post("/admin/login", data = "<form>")]
pub fn admin_login(
  form: Form<LoginForm>,
  cookies: &CookieJar<'_>,
  pool: &State<DbPool>,
) -> Redirect {
  let session_id = owner_for_token(&form.token).and_then(|owner| {
    let mut connection = pool.get().ok()?;
    Session::open(&mut connection, &owner, "token").ok()
  });
  match session_id {
    Some(session_id) => {
      cookies.add(
        Cookie::build((ADMIN_COOKIE, session_id))
          .http_only(true)
          .same_site(SameSite::Lax)
          .path("/")
          .build(),
      );
      Redirect::to(safe_next(form.next.as_deref()))
    },
    // Preserve the return destination across a failed attempt.
    None => Redirect::to(sign_in_url(true, form.next.as_deref())),
  }
}

/// Signs out (`POST /admin/logout`): **revokes** the server-side session (so the id is dead even if
/// the cookie lingers), clears the cookie, and returns to the sign-in page.
#[post("/admin/logout")]
pub fn admin_logout(cookies: &CookieJar<'_>, pool: &State<DbPool>) -> Redirect {
  if let Some(session_id) = cookies
    .get(ADMIN_COOKIE)
    .map(|cookie| cookie.value().to_string())
    && let Ok(mut connection) = pool.get()
  {
    let _ = Session::revoke(&mut connection, &session_id);
  }
  cookies.remove(Cookie::build(ADMIN_COOKIE).path("/").build());
  Redirect::to("/admin/login")
}

/// The route set for the admin web UI.
pub fn routes() -> Vec<Route> {
  routes![
    admin_page,
    admin_status_feed,
    admin_logs_feed,
    admin_login_page,
    admin_login,
    admin_logout
  ]
}
