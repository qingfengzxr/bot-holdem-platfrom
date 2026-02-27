#!/usr/bin/env bash
set -euo pipefail

# One-shot Postgres bootstrap for this repo:
# 1) create database if missing
# 2) run SQL migrations in order
# 3) print export command for DATABASE_URL
#
# Usage:
#   scripts/setup_postgres.sh
#   DB_NAME=bot_holdem_dev scripts/setup_postgres.sh
#   DATABASE_URL='postgres://postgres:postgres@127.0.0.1:5432/bot_holdem' scripts/setup_postgres.sh
#
# Optional env vars:
#   PGHOST (default: 127.0.0.1)
#   PGPORT (default: 5432)
#   PGUSER (default: postgres)
#   PGPASSWORD (optional)
#   DB_NAME (default: bot_holdem)
#   MAINTENANCE_DB (default: postgres)

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
MIGRATIONS_DIR="${ROOT_DIR}/migrations"

if ! command -v psql >/dev/null 2>&1; then
  echo "error: psql not found. Please install PostgreSQL client tools first." >&2
  exit 1
fi

PGHOST="${PGHOST:-127.0.0.1}"
PGPORT="${PGPORT:-5432}"
PGUSER="${PGUSER:-postgres}"
DB_NAME="${DB_NAME:-bot_holdem}"
MAINTENANCE_DB="${MAINTENANCE_DB:-postgres}"

replace_db_in_url() {
  local input_url="$1"
  local db_name="$2"
  # Replace only the path database segment, keep query string if present.
  printf '%s' "${input_url}" | sed -E "s#(postgres(ql)?://[^/]+/)[^?]*#\1${db_name}#"
}

if [[ -n "${DATABASE_URL:-}" ]]; then
  DB_URL="$(replace_db_in_url "${DATABASE_URL}" "${DB_NAME}")"
  DB_ADMIN_URL="${DATABASE_ADMIN_URL:-$(replace_db_in_url "${DATABASE_URL}" "${MAINTENANCE_DB}")}"
else
  DB_URL="postgres://${PGUSER}:${PGPASSWORD:-}@${PGHOST}:${PGPORT}/${DB_NAME}"
  DB_ADMIN_URL="postgres://${PGUSER}:${PGPASSWORD:-}@${PGHOST}:${PGPORT}/${MAINTENANCE_DB}"
fi

echo "==> Checking database: ${DB_NAME}"
DB_EXISTS="$(
  psql "${DB_ADMIN_URL}" \
    -tAc "SELECT 1 FROM pg_database WHERE datname='${DB_NAME}'" || true
)"

if [[ "${DB_EXISTS}" != "1" ]]; then
  echo "==> Creating database: ${DB_NAME}"
  psql "${DB_ADMIN_URL}" -v ON_ERROR_STOP=1 \
    -c "CREATE DATABASE \"${DB_NAME}\""
else
  echo "==> Database already exists: ${DB_NAME}"
fi

if [[ ! -d "${MIGRATIONS_DIR}" ]]; then
  echo "error: migrations directory not found: ${MIGRATIONS_DIR}" >&2
  exit 1
fi

echo "==> Running migrations from: ${MIGRATIONS_DIR}"
for f in "${MIGRATIONS_DIR}"/*.sql; do
  echo "   -> $(basename "${f}")"
  psql "${DB_URL}" -v ON_ERROR_STOP=1 -f "${f}"
done

echo
echo "Done."
echo "Use this in your shell:"
echo "  export DATABASE_URL='${DB_URL}'"
