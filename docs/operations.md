# Operations runbook

## Production gate

Do not expose this service to production traffic until every item in
[`release-blockers.md`](release-blockers.md) is closed. In particular, the
default binary has only a bounded in-memory receipt store. It refuses to start
unless `LIVY_RESOLVER_ALLOW_IN_MEMORY_RECEIPTS=true`, but that acknowledgment is
for a single-replica development deployment, not a durability guarantee.

A production application must call `run_with_receipt_store_factory` with a
shared durable `ReceiptStoreFactory`. The store must scope keys by tenant and
project, implement expiry, be safe under concurrent replicas, and make its
health visible through `health_check`. Build a deployment-specific binary from
that application before enabling more than one replica.

## Topology

Run the resolver behind a TLS-terminating gateway that:

- preserves `Authorization`, `Origin`, `Host`, `Idempotency-Key`, and
  `x-request-id`;
- rejects oversized requests before the resolver's 64 KiB application limit;
- enforces per-client and per-IP burst limits consistently across replicas;
- permits `/healthz` to the orchestrator, `/readyz` to the load balancer, and
  `/metrics` only to the metrics collector;
- sends traffic only to ready instances and allows at least
  `LIVY_RESOLVER_SHUTDOWN_GRACE_SECS` for termination.

The MCP transport is stateless, so it does not require sticky sessions. Product
receipts and credit idempotency do require shared backend state before a
multi-replica rollout. Never use source URLs, OAuth subjects, tenants, projects,
or receipt identifiers as gateway or metrics labels.

Allow outbound HTTPS only to the configured OAuth introspection service, Livy
backend, and the exact `SPIDER_API_URL` origin. The Spider endpoint must expose
the authenticated egress capability document described in the README on that
same origin. The resolver deliberately remains unready when that proof is
absent or invalid.

## Configuration and secrets

Start from `.env.example`, store real values in a secret manager, and inject
them at process start. Never commit `.env`, OAuth tokens, Spider keys, Livy API
keys, or ITA keys. Rotate a leaked credential before removing it from Git
history.

Required production decisions include:

- exact public MCP host and browser origin allow-lists;
- OAuth issuer, resource, audience, and method scopes;
- a shared durable receipt-store implementation;
- route prices and the backend credit reservation/capture policy;
- whether private provenance is enabled;
- whether the separately gated public disclosure capability is allowed;
- an actual Spider egress enforcement point and readiness URL.

Public provenance is irreversible. Keep visibility private, managed
publication off, response artifacts off, and public disclosure off unless a
specific data review approves them. Per-request consent is an additional gate,
not a replacement for deployment policy.

## Container

`Dockerfile` uses digest-pinned build and runtime images, builds with the locked
dependency graph, and runs as uid/gid `10001:10001`. The runtime filesystem can
be read-only and needs no writable working directory. Supply configuration as
environment variables; do not bake secrets into an image layer.

The checked-in image contains the default in-memory-store binary and therefore
is suitable for local or single-replica staging only. A production image must
replace the final binary with the deployment-specific durable-store wrapper.

## Health, drain, and rollout

- `GET /healthz` proves only that the process event loop is alive.
- `GET /readyz` checks accepting/draining state, receipt-store health, and the
  bounded Spider egress capability probe.
- `SIGTERM` first flips readiness to 503, cancels MCP work, and then drains the
  Axum server for the configured grace period.

Use a rolling deployment with a readiness probe, a liveness probe, and a
termination grace longer than the application grace. A failed readiness probe
must remove the instance from traffic without restarting it immediately;
repeated liveness failure can restart it.

## Observability

The service emits structured JSON events to stderr. Collect them with the
platform log agent and retain `request_id`, route, status, duration, and event
name. The code hashes source URLs and must never log raw bearer tokens, cookies,
request bodies, or source query strings.

`GET /metrics` exposes Prometheus text without high-cardinality labels:

- `livy_resolver_http_requests_total`
- `livy_resolver_http_requests_in_flight`
- `livy_resolver_http_responses_4xx_total`
- `livy_resolver_http_responses_5xx_total`
- `livy_resolver_http_request_duration_ms_total`

Scrape this endpoint on the private network. Derive a mean latency from the
duration and request counters; use gateway histograms for percentiles. Alert on
sustained readiness failure, 5xx rate, OAuth or credit backend failures,
upstream timeouts/oversize errors, receipt-store health failures, and elevated
gateway 429s. Treat a sudden loss of metrics as an availability incident.

## Scaling and recovery

Scale on in-flight work, upstream latency, and gateway queue depth rather than
CPU alone. The remote browser operations can consume capacity while the local
process is mostly idle. Keep request deadlines below the gateway deadline and
below the termination grace.

Back up the durable receipt store according to its evidence retention policy.
Receipt TTL and deletion must be consistent across replicas. Test restoration,
tenant isolation, and expiry before production. Credit state is authoritative
in the Livy backend and must not be reconstructed from resolver logs.

During an incident:

1. Remove affected instances from readiness or disable the route at the
   gateway.
2. Rotate exposed credentials and preserve redacted logs by request id.
3. Confirm whether a debit, receipt, or provenance publication occurred before
   retrying.
4. Never retry an irreversible public publication with a new idempotency key.
5. Record the cause and add a regression test before reopening traffic.
