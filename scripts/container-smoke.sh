#!/usr/bin/env bash
set -Eeuo pipefail

image="${1:-livy-resolver:ci}"
smoke_root="$(mktemp -d "${TMPDIR:-/tmp}/livy-resolver-container-smoke.XXXXXX")"
container_id=""

container_diagnostics() {
  if [[ -z "$container_id" ]]; then
    return
  fi

  {
    echo "container state:"
    docker inspect "$container_id" \
      --format 'status={{.State.Status}} running={{.State.Running}} exit_code={{.State.ExitCode}} oom_killed={{.State.OOMKilled}} error={{json .State.Error}}' || true
    echo "container logs:"
    docker logs "$container_id" || true
  } >&2
}

cleanup() {
  status=$?
  trap - EXIT
  if ((status != 0)); then
    container_diagnostics
  fi
  if [[ -n "$container_id" ]]; then
    docker rm --force "$container_id" >/dev/null 2>&1 || true
  fi
  rm -rf -- "$smoke_root"
  exit "$status"
}
trap cleanup EXIT

container_id="$(docker run --detach --publish '127.0.0.1::3001/tcp' --read-only \
  --cap-drop ALL --security-opt no-new-privileges \
  --env LIVY_RESOLVER_KEY=container-smoke-not-a-secret \
  --env LIVY_RESOLVER_AUTH_ENABLED=false \
  --env LIVY_RESOLVER_CREDITS_ENABLED=false \
  --env LIVY_RESOLVER_ALLOW_IN_MEMORY_RECEIPTS=true \
  "$image")"

mapfile -t port_mappings < <(docker port "$container_id" 3001/tcp)
if ((${#port_mappings[@]} != 1)); then
  printf 'expected exactly one IPv4 port mapping, got %d\n' "${#port_mappings[@]}" >&2
  printf 'mapping: %s\n' "${port_mappings[@]}" >&2
  exit 1
fi

if [[ "${port_mappings[0]}" =~ ^127\.0\.0\.1:([0-9]+)$ ]]; then
  host_port="${BASH_REMATCH[1]}"
else
  printf 'expected a loopback IPv4 mapping, got %q\n' "${port_mappings[0]}" >&2
  exit 1
fi
if ((host_port < 1 || host_port > 65535)); then
  printf 'Docker returned an invalid host port: %s\n' "$host_port" >&2
  exit 1
fi

base_url="http://127.0.0.1:${host_port}"
health_available=false
for _ in $(seq 1 100); do
  if curl --fail --silent --show-error --connect-timeout 1 --max-time 1 \
    --output "$smoke_root/health.json" "$base_url/healthz" \
    2>"$smoke_root/health-curl.log"; then
    health_available=true
    break
  fi
  if [[ "$(docker inspect "$container_id" --format '{{.State.Running}}')" != "true" ]]; then
    break
  fi
  sleep 0.1
done

if [[ "$health_available" != "true" ]]; then
  echo "container health endpoint did not become available at $base_url/healthz" >&2
  if [[ -s "$smoke_root/health-curl.log" ]]; then
    echo "last curl error:" >&2
    cat "$smoke_root/health-curl.log" >&2
  fi
  exit 1
fi

ruby -rjson -e '
  body = JSON.parse(File.read(ARGV.fetch(0)))
  abort "unexpected liveness response: #{body.inspect}" unless body == {"status" => "alive"}
' "$smoke_root/health.json"

ready_status="$(curl --silent --show-error --connect-timeout 2 --max-time 5 \
  --output "$smoke_root/ready.json" --write-out '%{http_code}' "$base_url/readyz")"
if [[ "$ready_status" != "503" ]]; then
  printf 'expected readiness status 503, got %s\n' "$ready_status" >&2
  exit 1
fi
ruby -rjson -e '
  body = JSON.parse(File.read(ARGV.fetch(0)))
  expected = {
    "status" => "not_ready",
    "dependencies" => "unavailable",
    "actual_fetch_egress" => "unavailable",
    "accepting_requests" => true,
  }
  abort "unexpected readiness response: #{body.inspect}" unless body == expected
' "$smoke_root/ready.json"

curl --fail --silent --show-error --connect-timeout 2 --max-time 5 \
  --output "$smoke_root/metrics.txt" "$base_url/metrics"
requests_total="$(awk '$1 == "livy_resolver_http_requests_total" { print $2 }' "$smoke_root/metrics.txt")"
requests_in_flight="$(awk '$1 == "livy_resolver_http_requests_in_flight" { print $2 }' "$smoke_root/metrics.txt")"
responses_5xx_total="$(awk '$1 == "livy_resolver_http_responses_5xx_total" { print $2 }' "$smoke_root/metrics.txt")"
if [[ ! "$requests_total" =~ ^[0-9]+$ ]] || ((requests_total < 3)); then
  printf 'invalid request total in metrics: %q\n' "$requests_total" >&2
  exit 1
fi
if [[ "$requests_in_flight" != "1" ]]; then
  printf 'invalid in-flight metric: %q\n' "$requests_in_flight" >&2
  exit 1
fi
if [[ ! "$responses_5xx_total" =~ ^[0-9]+$ ]] || ((responses_5xx_total < 1)); then
  printf 'invalid 5xx response total in metrics: %q\n' "$responses_5xx_total" >&2
  exit 1
fi

docker stop --time 15 "$container_id" >/dev/null
container_status="$(docker inspect "$container_id" --format '{{.State.Status}}')"
container_exit_code="$(docker inspect "$container_id" --format '{{.State.ExitCode}}')"
container_oom_killed="$(docker inspect "$container_id" --format '{{.State.OOMKilled}}')"
if [[ "$container_status" != "exited" || "$container_exit_code" != "0" || "$container_oom_killed" != "false" ]]; then
  printf 'container did not exit cleanly: status=%s exit_code=%s oom_killed=%s\n' \
    "$container_status" "$container_exit_code" "$container_oom_killed" >&2
  exit 1
fi
