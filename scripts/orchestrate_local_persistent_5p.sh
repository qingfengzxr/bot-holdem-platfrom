#!/usr/bin/env bash
set -euo pipefail

# One-click local stack:
# 1) start/reuse docker postgres
# 2) apply SQL migrations
# 3) start local anvil
# 4) start app-server with DATABASE_URL (postgres backend)
# 5) create one room and start 5 players in that room
#
# Usage:
#   scripts/orchestrate_local_persistent_5p.sh
# Optional env:
#   PG_CONTAINER_NAME (default: bot-holdem-pg)
#   PG_PORT (default: 55432)
#   PG_USER (default: postgres)
#   PG_PASSWORD (default: postgres)
#   PG_DB (default: bot_holdem)
#   ANVIL_PORT (default: 8545)
#   APP_RPC_PORT (default: 9000)
#   APP_OPS_PORT (default: 9100)
#   POLICY_MODE (default: codex_cli)

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
RUN_DIR="${ROOT_DIR}/.run/local_5p"
mkdir -p "${RUN_DIR}"

PG_CONTAINER_NAME="${PG_CONTAINER_NAME:-bot-holdem-pg}"
PG_PORT="${PG_PORT:-55432}"
PG_USER="${PG_USER:-postgres}"
PG_PASSWORD="${PG_PASSWORD:-postgres}"
PG_DB="${PG_DB:-bot_holdem}"

ANVIL_PORT="${ANVIL_PORT:-8545}"
APP_RPC_PORT="${APP_RPC_PORT:-9000}"
APP_OPS_PORT="${APP_OPS_PORT:-9100}"
POLICY_MODE="${POLICY_MODE:-codex_cli}"
ALLOW_PORT_AUTOFALLBACK="${ALLOW_PORT_AUTOFALLBACK:-true}"
CHAIN_VERIFY_MIN_CONFIRMATIONS="${CHAIN_VERIFY_MIN_CONFIRMATIONS:-1}"

RPC_ENDPOINT=""
WS_ENDPOINT=""
WALLET_RPC_URL=""
DATABASE_URL="postgres://${PG_USER}:${PG_PASSWORD}@127.0.0.1:${PG_PORT}/${PG_DB}"

ANVIL_LOG="${RUN_DIR}/anvil.log"
APP_LOG="${RUN_DIR}/app-server.log"
ROOM_ID_FILE="${RUN_DIR}/room_id.txt"

ANVIL_PID=""
APP_PID=""
PLAYER_PIDS=()

require_cmd() {
  local name="$1"
  if ! command -v "${name}" >/dev/null 2>&1; then
    echo "error: '${name}' not found in PATH" >&2
    exit 1
  fi
}

recompute_endpoints() {
  RPC_ENDPOINT="http://127.0.0.1:${APP_RPC_PORT}"
  WS_ENDPOINT="ws://127.0.0.1:${APP_RPC_PORT}"
  WALLET_RPC_URL="http://127.0.0.1:${ANVIL_PORT}"
}

is_port_available() {
  local port="$1"
  if command -v lsof >/dev/null 2>&1; then
    if lsof -nP -iTCP:"${port}" -sTCP:LISTEN >/dev/null 2>&1; then
      return 1
    fi
  fi
  python3 -c '
import socket,sys
port=int(sys.argv[1])
s=socket.socket(socket.AF_INET, socket.SOCK_STREAM)
try:
    s.bind(("0.0.0.0", port))
except OSError:
    raise SystemExit(1)
finally:
    s.close()
' "${port}"
}

choose_free_port() {
  local start_port="$1"
  local p
  for p in $(seq "${start_port}" "$((start_port + 200))"); do
    if is_port_available "${p}"; then
      echo "${p}"
      return 0
    fi
  done
  return 1
}

resolve_runtime_ports() {
  local new_port

  if ! is_port_available "${ANVIL_PORT}"; then
    if [[ "${ALLOW_PORT_AUTOFALLBACK}" != "true" ]]; then
      echo "error: ANVIL_PORT already in use: ${ANVIL_PORT}" >&2
      exit 1
    fi
    new_port="$(choose_free_port 18545)"
    echo "warn: ANVIL_PORT ${ANVIL_PORT} is occupied, fallback to ${new_port}" >&2
    ANVIL_PORT="${new_port}"
  fi

  if ! is_port_available "${APP_RPC_PORT}"; then
    if [[ "${ALLOW_PORT_AUTOFALLBACK}" != "true" ]]; then
      echo "error: APP_RPC_PORT already in use: ${APP_RPC_PORT}" >&2
      exit 1
    fi
    new_port="$(choose_free_port 19000)"
    echo "warn: APP_RPC_PORT ${APP_RPC_PORT} is occupied, fallback to ${new_port}" >&2
    APP_RPC_PORT="${new_port}"
  fi

  if ! is_port_available "${APP_OPS_PORT}" || [[ "${APP_OPS_PORT}" == "${APP_RPC_PORT}" ]]; then
    if [[ "${ALLOW_PORT_AUTOFALLBACK}" != "true" ]]; then
      echo "error: APP_OPS_PORT unavailable: ${APP_OPS_PORT}" >&2
      exit 1
    fi
    new_port="$(choose_free_port 19100)"
    echo "warn: APP_OPS_PORT ${APP_OPS_PORT} unavailable, fallback to ${new_port}" >&2
    APP_OPS_PORT="${new_port}"
  fi

  recompute_endpoints
  echo "==> runtime ports: anvil=${ANVIL_PORT} rpc=${APP_RPC_PORT} ops=${APP_OPS_PORT}"
}

docker_psql_retry() {
  local db="$1"
  shift
  local max_retries="${PSQL_RETRIES:-30}"
  local sleep_secs="${PSQL_RETRY_SLEEP_SECS:-1}"
  local i
  for i in $(seq 1 "${max_retries}"); do
    if docker exec -e PGPASSWORD="${PG_PASSWORD}" "${PG_CONTAINER_NAME}" \
      psql -U "${PG_USER}" -d "${db}" -v ON_ERROR_STOP=1 "$@"; then
      return 0
    fi
    sleep "${sleep_secs}"
  done
  docker exec -e PGPASSWORD="${PG_PASSWORD}" "${PG_CONTAINER_NAME}" \
    psql -U "${PG_USER}" -d "${db}" -v ON_ERROR_STOP=1 "$@"
}

docker_psql_file_retry() {
  local db="$1"
  local file="$2"
  local max_retries="${PSQL_RETRIES:-30}"
  local sleep_secs="${PSQL_RETRY_SLEEP_SECS:-1}"
  local i
  for i in $(seq 1 "${max_retries}"); do
    if docker exec -i -e PGPASSWORD="${PG_PASSWORD}" "${PG_CONTAINER_NAME}" \
      psql -U "${PG_USER}" -d "${db}" -v ON_ERROR_STOP=1 -f - < "${file}" >/dev/null 2>&1; then
      return 0
    fi
    sleep "${sleep_secs}"
  done
  docker exec -i -e PGPASSWORD="${PG_PASSWORD}" "${PG_CONTAINER_NAME}" \
    psql -U "${PG_USER}" -d "${db}" -v ON_ERROR_STOP=1 -f - < "${file}"
}

cleanup() {
  local code=$?
  for pid in "${PLAYER_PIDS[@]:-}"; do
    kill "${pid}" >/dev/null 2>&1 || true
  done
  if [[ -n "${APP_PID}" ]]; then
    kill "${APP_PID}" >/dev/null 2>&1 || true
  fi
  if [[ -n "${ANVIL_PID}" ]]; then
    kill "${ANVIL_PID}" >/dev/null 2>&1 || true
  fi
  wait >/dev/null 2>&1 || true
  exit "${code}"
}
trap cleanup INT TERM EXIT

start_or_reuse_postgres() {
  local exists running
  exists="$(docker ps -a --filter "name=^${PG_CONTAINER_NAME}$" --format "{{.Names}}" | head -n1 || true)"
  if [[ -z "${exists}" ]]; then
    echo "==> starting postgres container: ${PG_CONTAINER_NAME}"
    docker run -d \
      --name "${PG_CONTAINER_NAME}" \
      -e "POSTGRES_USER=${PG_USER}" \
      -e "POSTGRES_PASSWORD=${PG_PASSWORD}" \
      -e "POSTGRES_DB=${PG_DB}" \
      -p "${PG_PORT}:5432" \
      postgres:16 >/dev/null
  else
    running="$(docker ps --filter "name=^${PG_CONTAINER_NAME}$" --format "{{.Names}}" | head -n1 || true)"
    if [[ -z "${running}" ]]; then
      echo "==> starting existing postgres container: ${PG_CONTAINER_NAME}"
      docker start "${PG_CONTAINER_NAME}" >/dev/null
    else
      echo "==> postgres container already running: ${PG_CONTAINER_NAME}"
    fi
  fi

  echo "==> waiting for postgres ready"
  for _ in $(seq 1 60); do
    if docker exec "${PG_CONTAINER_NAME}" pg_isready -U "${PG_USER}" >/dev/null 2>&1; then
      return
    fi
    sleep 1
  done
  echo "error: postgres did not become ready" >&2
  exit 1
}

apply_migrations() {
  local migrations_dir db_exists
  migrations_dir="${ROOT_DIR}/migrations"
  if [[ ! -d "${migrations_dir}" ]]; then
    echo "error: migrations directory not found: ${migrations_dir}" >&2
    exit 1
  fi
  echo "==> ensuring database exists: ${PG_DB}"
  db_exists="$(
    docker_psql_retry postgres -tAc "SELECT 1 FROM pg_database WHERE datname='${PG_DB}'" || true
  )"
  db_exists="$(echo "${db_exists}" | tr -d '[:space:]')"
  if [[ "${db_exists}" != "1" ]]; then
    docker_psql_retry postgres -c "DO \$\$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_database WHERE datname='${PG_DB}') THEN
    EXECUTE 'CREATE DATABASE \"${PG_DB}\"';
  END IF;
END
\$\$;"
  fi

  echo "==> applying migrations"
  for f in "${migrations_dir}"/*.sql; do
    echo "   -> $(basename "${f}")"
    docker_psql_file_retry "${PG_DB}" "${f}"
  done
}

wait_http_jsonrpc_ready() {
  local url="$1"
  local pid="${2:-}"
  for _ in $(seq 1 80); do
    if [[ -n "${pid}" ]] && ! kill -0 "${pid}" >/dev/null 2>&1; then
      return 2
    fi
    if python3 -c '
import json,sys,urllib.request
url=sys.argv[1]
req=urllib.request.Request(
    url,
    data=json.dumps({"jsonrpc":"2.0","id":1,"method":"room.list","params":{"include_inactive":True}}).encode(),
    headers={"Content-Type":"application/json"},
    method="POST",
)
try:
    with urllib.request.urlopen(req, timeout=2) as r:
        body=json.loads(r.read().decode())
    ok = isinstance(body, dict) and body.get("jsonrpc") == "2.0" and "result" in body
    raise SystemExit(0 if ok else 1)
except Exception:
    raise SystemExit(1)
' "${url}"; then
      return
    fi
    sleep 0.25
  done
  return 1
}

jsonrpc_http_call() {
  local url="$1"
  local method="$2"
  local params_json="$3"
  local payload http_code body_file
  payload="{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"${method}\",\"params\":${params_json}}"
  body_file="$(mktemp "${RUN_DIR}/jsonrpc.XXXXXX.body")"
  http_code="$(
    curl -sS -o "${body_file}" -w "%{http_code}" \
      -X POST "${url}" \
      -H "content-type: application/json" \
      --data "${payload}"
  )"
  cat "${body_file}"
  rm -f "${body_file}"
  if [[ "${http_code}" != "200" ]]; then
    return 22
  fi
}

start_anvil() {
  echo "==> starting anvil on :${ANVIL_PORT}"
  anvil -p "${ANVIL_PORT}" --silent >"${ANVIL_LOG}" 2>&1 &
  ANVIL_PID=$!
  for _ in $(seq 1 80); do
    if curl -sS -X POST "${WALLET_RPC_URL}" \
      -H "content-type: application/json" \
      -d '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}' >/dev/null 2>&1; then
      return
    fi
    sleep 0.25
  done
  echo "error: anvil not ready on ${WALLET_RPC_URL}" >&2
  exit 1
}

start_app_server() {
  echo "==> starting app-server with postgres backend"
  echo "==> chain verify: endpoint=${WALLET_RPC_URL} min_confirmations=${CHAIN_VERIFY_MIN_CONFIRMATIONS}"
  DATABASE_URL="${DATABASE_URL}" \
  APP_SERVER__AUTO_SETTLEMENT_ENABLED=true \
  SETTLEMENT_EVM_RPC_URL="${WALLET_RPC_URL}" \
  APP_SERVER__CHAIN_VERIFY_RPC_ENDPOINT="${WALLET_RPC_URL}" \
  APP_SERVER__CHAIN_VERIFY_MIN_CONFIRMATIONS="${CHAIN_VERIFY_MIN_CONFIRMATIONS}" \
  APP_SERVER__RPC_BIND_ADDR="0.0.0.0:${APP_RPC_PORT}" \
  APP_SERVER__OPS_HTTP_BIND_ADDR="0.0.0.0:${APP_OPS_PORT}" \
  cargo run -p app-server >"${APP_LOG}" 2>&1 &
  APP_PID=$!
  if wait_http_jsonrpc_ready "${RPC_ENDPOINT}" "${APP_PID}" >/dev/null 2>&1; then
    echo "==> app-server ready: ${RPC_ENDPOINT} (ops: http://127.0.0.1:${APP_OPS_PORT})"
    return
  fi
  if ! kill -0 "${APP_PID}" >/dev/null 2>&1; then
    echo "error: app-server process exited before ready" >&2
  else
    echo "error: app-server rpc not ready in time: ${RPC_ENDPOINT}" >&2
  fi
  echo "app-server log tail:" >&2
  tail -n 120 "${APP_LOG}" >&2 || true
  exit 1
}

create_room() {
  local room_id create_resp list_resp
  create_resp="$(jsonrpc_http_call "${RPC_ENDPOINT}" "room.create" "[]")" || true
  room_id="$(
    python3 -c '
import json,sys
raw=sys.stdin.read().strip()
if not raw:
    print("")
    raise SystemExit(0)
try:
    body=json.loads(raw)
    result=body.get("result",{})
    data=result.get("data",{})
    rid=data.get("room_id","")
    print(rid if isinstance(rid,str) else "")
except Exception:
    print("")
' <<< "${create_resp}"
  )"
  if [[ -z "${room_id}" ]]; then
    echo "warn: room.create did not return room_id, fallback to room.list" >&2
    if [[ -n "${create_resp}" ]]; then
      echo "room.create response: ${create_resp}" >&2
    fi
    list_resp="$(jsonrpc_http_call "${RPC_ENDPOINT}" "room.list" '{"include_inactive":true}')" || true
    room_id="$(
      python3 -c '
import json,sys
raw=sys.stdin.read().strip()
if not raw:
    print("")
    raise SystemExit(0)
try:
    body=json.loads(raw)
    rooms=body.get("result",{}).get("data",[])
    if isinstance(rooms,list) and rooms:
        for room in rooms:
            if isinstance(room,dict) and room.get("status","").lower()=="active" and isinstance(room.get("room_id"),str):
                print(room["room_id"]); raise SystemExit(0)
        first=rooms[0]
        if isinstance(first,dict) and isinstance(first.get("room_id"),str):
            print(first["room_id"]); raise SystemExit(0)
    print("")
except Exception:
    print("")
' <<< "${list_resp}"
    )"
  fi
  if [[ -z "${room_id}" ]]; then
    echo "error: failed to resolve room id via room.create/room.list" >&2
    echo "app-server log tail:" >&2
    tail -n 80 "${APP_LOG}" >&2 || true
    exit 1
  fi
  printf '%s\n' "${room_id}" >"${ROOM_ID_FILE}"
  echo "==> room created: ${room_id}"
}

start_players() {
  local room_id seat address player_log
  room_id="$(cat "${ROOM_ID_FILE}")"
  echo "==> starting 5 players in room ${room_id}"
  for seat in 0 1 2 3 4; do
    address="$(
      python3 -c '
import json,sys,urllib.request
url=sys.argv[1]
idx=int(sys.argv[2])
req=urllib.request.Request(url,data=json.dumps({"jsonrpc":"2.0","id":1,"method":"eth_accounts","params":[]}).encode(),headers={"Content-Type":"application/json"},method="POST")
with urllib.request.urlopen(req,timeout=10) as r:
    body=json.loads(r.read().decode())
print(body["result"][idx])
' "${WALLET_RPC_URL}" "${seat}"
    )"
    player_log="${RUN_DIR}/player-${seat}.log"
    echo "   -> seat ${seat} address ${address}"
    scripts/run_single_seat_codex.py \
      --rpc-endpoint "${RPC_ENDPOINT}" \
      --wallet-rpc-url "${WALLET_RPC_URL}" \
      --room-id "${room_id}" \
      --seat-id "${seat}" \
      --seat-address "${address}" \
      >"${player_log}" 2>&1 &
    PLAYER_PIDS+=("$!")
    sleep 0.3
  done
}

print_summary() {
  local room_id
  room_id="$(cat "${ROOM_ID_FILE}")"
  cat <<EOF

Stack started successfully.
  DATABASE_URL=${DATABASE_URL}
  Room ID: ${room_id}
  RPC: ${RPC_ENDPOINT}
  WS: ${WS_ENDPOINT}
  Wallet RPC: ${WALLET_RPC_URL}

Logs:
  anvil: ${ANVIL_LOG}
  app-server: ${APP_LOG}
  players: ${RUN_DIR}/player-0.log ... ${RUN_DIR}/player-4.log

Press Ctrl-C to stop app-server/anvil/players.
Postgres container is kept: ${PG_CONTAINER_NAME}
EOF
}

main() {
  require_cmd docker
  require_cmd cargo
  require_cmd curl
  require_cmd python3
  require_cmd anvil

  cd "${ROOT_DIR}"
  resolve_runtime_ports
  start_or_reuse_postgres
  apply_migrations
  start_anvil
  start_app_server
  create_room
  start_players
  print_summary
  wait
}

main "$@"
