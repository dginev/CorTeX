#!/usr/bin/env bash
#
# verify_migrations.sh — does migrations/ reproduce a database's schema?
#
# Builds a fresh REFERENCE database by running every migration in migrations/ on an empty DB, then
# diffs its schema (--schema-only) against a SOURCE database. A non-empty diff means the source has
# schema our migrations do NOT reproduce (a migration to author / manual drift), or the source is
# behind our migrations. This is the productization-blocking "where are we missing migrations?"
# check from docs/archive/LOAD_TESTING.md — point it at a restored live-backup DB.
#
# Usage:
#   scripts/verify_migrations.sh <source_db_url> [<ref_db_url>]
#
# Env (defaults suit the dev box):
#   PSQL_ADMIN   command that can CREATE/DROP DATABASE   (default: "sudo -u postgres")
#   REF_OWNER    role to own the reference DB            (default: cortex)
#
# Validate the tool itself: run it with the up-to-date `cortex` DB as the source — it should report
# OK (the live, manually-migrated DB matches a fresh migration run):
#   scripts/verify_migrations.sh postgres://cortex:cortex@localhost/cortex
#
# Exit: 0 = schemas match, 1 = drift (differences printed), 2 = usage/precondition error.
set -euo pipefail
# Force a byte-order locale so `sort` and `comm` agree on collation (otherwise comm warns
# "not in sorted order" and the set comparison is unreliable).
export LC_ALL=C

SOURCE_URL="${1:-}"
REF_URL="${2:-postgres://cortex:cortex@localhost/cortex_migration_ref}"
PSQL_ADMIN="${PSQL_ADMIN:-sudo -u postgres}"
REF_OWNER="${REF_OWNER:-cortex}"

if [[ -z "$SOURCE_URL" ]]; then
  echo "usage: scripts/verify_migrations.sh <source_db_url> [<ref_db_url>]" >&2
  exit 2
fi
if [[ ! -d migrations ]]; then
  echo "error: run from the repository root (no ./migrations directory found)" >&2
  exit 2
fi
command -v diesel >/dev/null || { echo "error: diesel CLI not found (cargo install diesel_cli)" >&2; exit 2; }

# Reference DB name = last path segment of REF_URL (sans any ?query).
REF_DB="$(basename "${REF_URL%%\?*}")"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "-- Rebuilding reference DB '$REF_DB' (owner $REF_OWNER) from migrations/ ..."
$PSQL_ADMIN psql -v ON_ERROR_STOP=1 -q -c "DROP DATABASE IF EXISTS \"$REF_DB\";"
$PSQL_ADMIN psql -v ON_ERROR_STOP=1 -q -c "CREATE DATABASE \"$REF_DB\" OWNER \"$REF_OWNER\";"
diesel migration run --database-url "$REF_URL" --migration-dir migrations >/dev/null
echo "-- Reference built."

# Schema dump with the noise filtered out (ownership/privileges/comments/SET lines), so the diff is
# pure DDL. Sorted-line comparison makes it robust to pg_dump's object-ordering differences between
# two databases — it surfaces every added/removed/changed DDL line regardless of emission order.
dump_schema() {
  pg_dump --schema-only --no-owner --no-privileges --no-comments "$1" \
    | grep -vE '^(SET |SELECT pg_catalog|--|$|\\)' \
    | sort -u
}

echo "-- Dumping schemas (source vs reference) ..."
dump_schema "$SOURCE_URL" > "$TMP/source.sql"
dump_schema "$REF_URL"    > "$TMP/ref.sql"

# Lines only in SOURCE  -> schema the source has that our migrations don't reproduce (author one).
# Lines only in REF     -> schema our migrations produce that the source lacks (source is behind).
ONLY_SOURCE="$(comm -23 "$TMP/source.sql" "$TMP/ref.sql" || true)"
ONLY_REF="$(comm -13 "$TMP/source.sql" "$TMP/ref.sql" || true)"

if [[ -z "$ONLY_SOURCE" && -z "$ONLY_REF" ]]; then
  echo "OK: the source schema matches migrations/ — no drift, no missing migrations."
  exit 0
fi

echo "DRIFT: the source schema differs from what migrations/ produces."
if [[ -n "$ONLY_SOURCE" ]]; then
  echo
  echo "### In SOURCE but NOT reproduced by migrations/ (author a migration for these):"
  echo "$ONLY_SOURCE"
fi
if [[ -n "$ONLY_REF" ]]; then
  echo
  echo "### Produced by migrations/ but MISSING from SOURCE (source is behind — run migrations):"
  echo "$ONLY_REF"
fi
exit 1
