#!/usr/bin/env bash
set -euo pipefail

repo_root="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
binary="${1:-$repo_root/target/debug/livy-resolver}"
smoke_port="${SMOKE_PORT:-39091}"
smoke_root="$(mktemp -d "${TMPDIR:-/tmp}/livy-resolver-smoke.XXXXXX")"
server_pid=""

cleanup() {
  if [[ -n "$server_pid" ]] && kill -0 "$server_pid" 2>/dev/null; then
    kill -TERM "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -rf -- "$smoke_root"
}
trap cleanup EXIT

if [[ ! -x "$binary" ]]; then
  cargo build --manifest-path "$repo_root/Cargo.toml" --locked
fi

env \
  PORT="$smoke_port" \
  LIVY_RESOLVER_KEY="smoke-only-not-a-secret" \
  LIVY_RESOLVER_AUTH_ENABLED=false \
  LIVY_RESOLVER_CREDITS_ENABLED=false \
  LIVY_RESOLVER_ALLOW_IN_MEMORY_RECEIPTS=true \
  LIVY_PROVENANCE_ENABLED=false \
  "$binary" >"$smoke_root/server.log" 2>&1 &
server_pid="$!"

base_url="http://127.0.0.1:$smoke_port"
for _ in $(seq 1 50); do
  if curl --fail --silent "$base_url/healthz" >"$smoke_root/health.json"; then
    break
  fi
  if ! kill -0 "$server_pid" 2>/dev/null; then
    cat "$smoke_root/server.log" >&2
    exit 1
  fi
  sleep 0.1
done

grep -qF '"status":"alive"' "$smoke_root/health.json"

ready_status="$(curl --silent --output "$smoke_root/ready.json" --write-out '%{http_code}' "$base_url/readyz")"
test "$ready_status" = "503"
grep -qF '"status":"not_ready"' "$smoke_root/ready.json"

invalid_status="$(curl --silent --output "$smoke_root/invalid.json" --write-out '%{http_code}' \
  -H 'content-type: application/json' \
  --data '{"source":"https://example.com","unknown_contract_field":true}' \
  "$base_url/fetch")"
test "$invalid_status" = "400"
grep -qF '"code":"invalid_json"' "$smoke_root/invalid.json"

curl --fail --silent --show-error "$base_url/metrics" >"$smoke_root/metrics.txt"
grep -Eq '^livy_resolver_http_requests_total [3-9][0-9]*$' "$smoke_root/metrics.txt"
grep -Eq '^livy_resolver_http_requests_in_flight 1$' "$smoke_root/metrics.txt"

kill -TERM "$server_pid"
wait "$server_pid"
server_pid=""
