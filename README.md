# Livy Web Resolver

Default mode is SmartMode. Override via `mode` when you need specific
cost/speed/browser/unblock behavior.

This repository now has reproducible build and security gates, but it is not
approved for public distribution or multi-replica production until the tracked
[`release blockers`](docs/release-blockers.md) are closed.

## Capabilities

- Browser and browserless URL resolution through bounded Spider operations
- Strict product request validation and adaptive in-request fallback
- OAuth-protected HTTP and stateless MCP surfaces
- Tenant-scoped receipts, credit capture, and optional private provenance
- Fail-closed source egress policy, readiness, graceful drain, and metrics

## Setup

```dotenv
LIVY_RESOLVER_KEY=your-resolver-key
LIVY_RESOLVER_AUTH_ENABLED=false
LIVY_RESOLVER_CREDITS_ENABLED=false
LIVY_RESOLVER_ALLOW_IN_MEMORY_RECEIPTS=true
```

```bash
cargo run
```

`SPIDER_API_KEY`, `SPIDER_KEY`, and `LIVY_KEY` remain accepted as legacy
aliases for `LIVY_RESOLVER_KEY`.
The service listens on `http://localhost:3001` unless `PORT` or
`RESOLVER_PORT` is set.

Copy `.env.example` to `.env` for the full local configuration template.
Its fail-closed default rejects the in-memory receipt store; set
`LIVY_RESOLVER_ALLOW_IN_MEMORY_RECEIPTS=true` only for a local or single-replica
staging process. A source fetch also remains unready until the actual Spider
egress capability documented under API security is configured.
The exact Rust 1.94.0 toolchain is selected by `rust-toolchain.toml`. A sibling
`livy-core` checkout is neither required nor read; the small provenance HTTP
boundary is owned by this repository.

The exact HTTP schema is [`docs/openapi.yaml`](docs/openapi.yaml). See the
[`operations runbook`](docs/operations.md),
[`dependency policy`](docs/dependency-policy.md), and
[`MCP client example`](docs/mcp-client.example.json) before deployment.

Product routes and MCP requests require a Livy OAuth bearer token by default.
Unauthenticated MCP requests, including `initialize` and `tools/list`, return
HTTP `401` with `WWW-Authenticate` so Claude can discover the OAuth protected
resource and show the sign-in flow. Local-only unauthenticated development can
set `LIVY_RESOLVER_AUTH_ENABLED=false`.
When authentication is disabled for local development, also explicitly set
`LIVY_RESOLVER_CREDITS_ENABLED=false`; enabled credit enforcement requires an
authenticated access token and fails closed without one.

```dotenv
LIVY_OAUTH_ISSUER=https://auth.livylabs.xyz
LIVY_OAUTH_INTROSPECTION_URL=https://auth.livylabs.xyz/oauth/introspect
LIVY_RESOLVER_OAUTH_AUDIENCE=https://resolver.api.livylabs.xyz/mcp
LIVY_RESOLVER_OAUTH_RESOURCE_METADATA_URL=https://resolver.api.livylabs.xyz/.well-known/oauth-protected-resource
```

Introspection responses must contain the configured issuer, the configured MCP
audience, the Livy tenant/project claims, and the exact route scope. Alternate
audiences are accepted only when explicitly listed in
`LIVY_RESOLVER_OAUTH_ACCEPTED_AUDIENCES`; the legacy audience is not implicit.

## Livy Provenance

The resolver can store a Livy provenance attestation for every successful
fetch. This branch records generic resolver source-fetch proofs with:

- `attestation_claim=source`
- `subject_type=resolver_fetch`
- `schema_id=resolver-fetch-v1`, `schema_version=1` (the current backend contract)
- the public `source` field contains only a stable `sha256:<digest>` commitment,
  never the raw URL or query
- `integration_id=delphi` by default

This is intentionally not the `prediction_market_resolver` template. Use
a market-resolution template only when the resolver emits market outcome
fields such as market id, winning outcome, confidence, and settlement
target.

Enable provenance with:

```dotenv
LIVY_RESOLVER_KEY=your-resolver-key
LIVY_PROVENANCE_ENABLED=true
LIVY_BACKEND_BASE_URL=https://api.livylabs.xyz
LIVY_INTEGRATION_ID=delphi
ITA_API_KEY=...
```

For OAuth-protected MCP `fetch_source` calls, provenance writes are posted to
`/api/v1/resolver/source-fetch-attestations` with the user's Livy OAuth bearer
token. The backend derives tenant/project from the token and rejects
project-less resolver tokens. Do not grant ChatGPT clients generic
`provenance:attestation:write`.

Optional settings:

```dotenv
LIVY_API_KEY=livy_...
LIVY_PROVENANCE_SCHEMA_ID=resolver-fetch-v1
LIVY_PROVENANCE_SCHEMA_VERSION=1
LIVY_PROVENANCE_VISIBILITY=private
LIVY_PROVENANCE_VERIFICATION_MODE=verify_fresh
LIVY_EXPLORER_BASE_URL=https://api.livylabs.xyz
LIVY_PROVENANCE_BOOTSTRAP_TEMPLATE=false
LIVY_PROVENANCE_MANAGED_PUBLICATION=false
LIVY_PROVENANCE_PUBLISH_RESPONSE_ARTIFACT=false
LIVY_PROVENANCE_ALLOW_PUBLIC_DISCLOSURE=false
LIVY_PROVENANCE_RESPONSE_ARTIFACT_MAX_BYTES=262144
LIVY_PROVENANCE_WAIT_FOR_REGISTRY_REFS=false
LIVY_PROVENANCE_REGISTRY_WAIT_ATTEMPTS=30
LIVY_PROVENANCE_REGISTRY_WAIT_INTERVAL_MS=2000
LIVY_PROVENANCE_TIMEOUT_SECS=10
```

`LIVY_PROVENANCE_ENABLED=true` is the only setting that activates provenance;
shared backend URLs and service credentials never activate it implicitly.
`LIVY_BACKEND_BASE_URL` defaults to `https://api.livylabs.xyz`; set it for local
or staging backends. It may contain a reverse-proxy deployment prefix such as
`https://example.test/livy`, which is preserved when the resolver appends its
fixed endpoint paths. Do not include `/api/v1`, credentials, a query, or a
fragment in the base URL.

The current Livy backend accepts only `resolver-fetch-v1@1`; the resolver
fails startup on any other configured schema instead of making incompatible
calls. A future v2 schema requires a coordinated external backend registry and
API rollout before this client can adopt it.

`LIVY_API_KEY` is only used for legacy/local service-key provenance writes.
Production service-key writes are disabled unless
`LIVY_PROVENANCE_ALLOW_SERVICE_API_KEY=true` is set.

Set `LIVY_PROVENANCE_BOOTSTRAP_TEMPLATE=true` only when the API key is
allowed to write provenance templates. Public explorer reads also require
the matching public template to exist in Livy.

`LIVY_PROVENANCE_MANAGED_PUBLICATION=true` asks Livy backend to publish the
provenance receipt and a versioned resolver request/response exchange to
Arweave, then register the receipt on the configured EVM registry. The exchange
is named `resolver-response.json` and uses the `resolver-tool-exchange-v1`
schema. Its top-level `request` contains the same sanitized request summary used
for the input commitment, while `response` contains the exact upstream JSON.
The `commitments.request_sha256` and `commitments.response_sha256` values bind
those fields to the attestation. Header and cookie values are never revealed;
the request summary records only their presence or count. Artifacts larger than
`LIVY_PROVENANCE_RESPONSE_ARTIFACT_MAX_BYTES` remain commitment-only so an
oversized reveal cannot block receipt publication. Set
`LIVY_PROVENANCE_PUBLISH_RESPONSE_ARTIFACT=false` to keep every response
commitment-only. Response artifacts and managed publication default off, and
the source URL is a commitment field rather than a public value. Public
visibility or managed publication additionally requires the explicit
`LIVY_PROVENANCE_ALLOW_PUBLIC_DISCLOSURE=true` deployment capability and an
authenticated per-request `provenance.publish=true` consent. Revealing the
upstream response additionally requires per-request
`provenance.reveal_response=true`. Both request values default to false; a
deployment flag alone never publishes or reveals data. Managed publication is
public and irreversible, so enable it only for resolver outputs that are safe
to disclose. The resolver
still returns the attestation immediately. Set
`LIVY_PROVENANCE_WAIT_FOR_REGISTRY_REFS=true` only when the caller should wait
for public `registry_refs`; that mode requires the API key to have provenance
read access in addition to write access.

## HTTP API

Response shape:

```json
{
  "route": "...",
  "mode": "...",
  "receipt_id": "...",
  "receipt": {},
  "data": {},
  "provenance": {
    "provenance_attestation_id": "...",
    "subject_id": "resolver_fetch:...",
    "schema_id": "resolver-fetch-v1",
    "verification_status": "verified",
    "schema_binding_status": "full",
    "explorer_url": "...",
    "managed_publication": {
      "status": "publishing"
    },
    "registry_refs": []
  }
}
```

Request fields include `source`, `query`/`q`, `mode`, `format`, `proxy`, and
`receipt`, plus optional consent
`provenance: { "publish": false, "reveal_response": false }`. The mode
contract is route-specific:

- `/fetch` accepts `auto`, `fast`, `browser`, `unblock`, or `raw`.
- `/crawl`, `/map`, `/search`, `/extract`, and `/screenshot` accept `auto` or
  their route-named mode. Other combinations return `400` instead of running a
  different operation under the requested label.
- `proxy` accepts `auto`, `none`, `isp`, `residential`, or `mobile`.
  `proxy_enabled` is deprecated and rejected. `auto` uses ISP for
  auto/fast/extract/unblock and no proxy for the other modes.

Auto and fast requests perform at most one unblock fallback during the current
request when the first response is a recognized browser, JavaScript, robots, or
human-verification challenge. HTTP or payload status `401`, `403`, or `429` can
be challenge-eligible; redirects and `5xx` responses are always terminal even
when their body contains challenge text. Both attempts share one deadline and
only the final successful result can create a receipt or provenance record.

Authenticated requests use this billing order: validate the request, perform a
non-consuming balance/entitlement preflight, obtain and validate a pending
Spider result, capture the idempotent credit debit, then create the receipt and
provenance record and return the result. Insufficient balance therefore prevents
Spider work, while an upstream HTTP/payload failure, timeout, or oversized body
cannot produce a captured debit, receipt, or provenance record. A capture
failure after successful Spider work also prevents finalization.

The current backend does not expose atomic reserve/capture/cancel operations.
Consequently, the balance can change after preflight, and some valid Spider work
can be consumed before an authoritative capture loses that race. Closing that
cost-abuse window requires a backend reservation before Spider, followed by
capture on success or cancellation on failure.

`Idempotency-Key` is scoped to the authenticated tenant, project, and client and
bound to a canonical fingerprint of the full execution plan. The resolver fails
closed with `409` when a completed key is replayed because the credit backend can
return the prior debit but cannot return or authoritatively bind the prior
resolver result, receipt, and provenance. The complete backend enhancement is an
atomic operation API that reserves a caller key and fingerprint, captures or
cancels that reservation, and returns the original resolver result/evidence
reference on replay. Until it exists, the resolver uses a bounded local conflict
registry and accepts only a debit response carrying a ledger record bound to
the exact request fingerprint, pricing version, amount, project, idempotency
key, and unique capture attempt. It never treats a generic ledger listing as
proof that work may proceed. Cross-process caller-key races and an ambiguous
capture without a caller key remain residual limitations.

| Method | Path | Purpose |
|---|---|---|
| POST | `/fetch` | Fetch one URL |
| POST | `/crawl` | Crawl with `limit`, `depth` |
| POST | `/map` | Discover links |
| POST | `/search` | Web search (+ optional page fetch) |
| POST | `/extract` | Extraction with selectors |
| POST | `/screenshot` | Capture screenshot |
| POST | `/snapshot` | Capture raw HTML and screenshot with one resolver receipt |
| POST | `/fetchfast` | Compat: fast fetch |
| POST | `/fetchunblock` | Compat: unblock fetch |
| GET | `/receipt/{id}` | Read receipt |
| GET | `/healthz` | Process liveness |
| GET | `/readyz` | Dependency and drain readiness |
| GET | `/metrics` | Private-network Prometheus process metrics |
| GET | `/.well-known/oauth-protected-resource[/mcp]` | OAuth resource metadata |
| POST | `/mcp` | Preferred stateless MCP JSON-RPC endpoint |
| POST | `/` | Deprecated compatibility alias for `/mcp` |

Prefer `/fetch` with `mode` over the compat routes.

Send one validated `Idempotency-Key` header on product requests and reuse it
only when retrying the same logical request. The resolver scopes and hashes the
key with the authenticated OAuth subject, tenant, project, client, and route
before sending it to the credit service. OAuth introspection must therefore
return a non-empty `sub`. The backend key is bound to a canonical fingerprint of
the complete validated execution plan, caller key, authenticated scope,
effective amount, and pricing version. Reusing a caller key for a different
request returns HTTP 409 within a replica; across replicas the different
fingerprint produces a different debit key, so it cannot reuse the first
request's debit. Once a replica has captured a request, retrying that same key
returns HTTP 409 before Spider because the resolver has no durable prior-result
cache. After a restart, a generic credit-ledger row is never accepted as proof
of an enforced capture: insufficient balance still fails before Spider.
`LIVY_RESOLVER_REQUEST_CREDIT_COST` is the default price; deployments can set
route-specific overrides such as `LIVY_RESOLVER_CREDIT_COST_CRAWL`,
`LIVY_RESOLVER_CREDIT_COST_SCREENSHOT`, and
`LIVY_RESOLVER_CREDIT_COST_MCP_FETCH_SOURCE`. The resolver performs a
non-consuming balance preflight before Spider, executes the upstream request,
captures the debit only after upstream success, and only then stores a receipt
or writes provenance. Failed upstream attempts are not debited. Credit
responses that are neither enforced nor an idempotent replay fail closed.

The current backend does not provide an atomic reserve/capture operation or a
transactional caller-key-to-fingerprint conflict check. A backend enhancement
must atomically reserve sufficient balance with the caller-key scope and
request fingerprint, return 409 `idempotency_conflict` for a different
fingerprint, and capture/release that reservation after success/failure. Until
that rollout, the GET preflight has an unavoidable concurrent-spend race; the
final debit still fails closed before receipt/provenance side effects.

Receipt identifiers are random and lookup is scoped to the authenticated
tenant and project. The default in-memory store expires records after 15
minutes and keeps at most 10,000 records. It is not durable or replica-shared,
so every environment refuses it unless
`LIVY_RESOLVER_ALLOW_IN_MEMORY_RECEIPTS=true` explicitly acknowledges a
single-replica exception. This repository does not claim or ship a durable
implementation; applications can inject an async shared durable `ReceiptStore`
through `ReceiptStoreFactory`. The library exports
`build_app_with_receipt_store_factory` and `run_with_receipt_store_factory` so
an application can supply that factory without using the default environment
factory. Factory creation and store health checks are async; startup and
`/readyz` fail closed when the supplied store is unavailable.

## API security

Product requests are limited to 64 KiB and 65 seconds by default. Override
these deployment safety limits with:

```dotenv
LIVY_RESOLVER_MAX_PRODUCT_BODY_BYTES=65536
LIVY_RESOLVER_PRODUCT_TIMEOUT_SECS=65
LIVY_RESOLVER_MAX_UPSTREAM_BYTES=8388608
LIVY_RESOLVER_MCP_TIMEOUT_SECS=65
LIVY_RESOLVER_SHUTDOWN_GRACE_SECS=10
LIVY_RESOLVER_HSTS_ENABLED=false
LIVY_RESOLVER_DNS_TIMEOUT_SECS=3
LIVY_RESOLVER_ALLOW_PRIVATE_SOURCES=false
# Required in production before any Spider-backed source operation is accepted:
SPIDER_API_URL=https://api.spider.cloud
LIVY_RESOLVER_SPIDER_EGRESS_ATTESTATION=spider-egress-policy-v1
LIVY_RESOLVER_SPIDER_EGRESS_READINESS_URL=https://api.spider.cloud/egress-capability
LIVY_RESOLVER_SPIDER_EGRESS_READINESS_TIMEOUT_SECS=3
LIVY_RESOLVER_ALLOW_INSECURE_SPIDER_DEV=false
```

Spider calls have a five-second connect timeout, a 65-second client ceiling,
the request's `timeout_secs` absolute deadline (1–60 seconds), and the response
limit above. Redirect following is disabled, so control-plane 3xx responses are
errors even when their target would return 2xx. Every non-2xx response and every
payload item whose source `status` is outside 200–299 maps to an upstream error
before credit capture, receipt creation, or provenance creation.

Enable HSTS only when the public endpoint is served through HTTPS. The API
accepts absolute HTTP and HTTPS source URLs that resolve exclusively to public
addresses. Literal and DNS-resolved loopback, private, carrier-grade NAT,
link-local, documentation, benchmarking, transition, multicast, reserved,
IPv4-embedded metadata, and cloud-metadata destinations are rejected before
credit debit. Mixed public/private DNS answers also fail closed and local DNS is
checked on every request.

Those local checks cannot bind the address used by the remote Spider service.
Consequently, all Spider-backed operations (including search) fail with 503 by
default. Production must set the exact
`LIVY_RESOLVER_SPIDER_EGRESS_ATTESTATION=spider-egress-policy-v1` value and a
bounded `LIVY_RESOLVER_SPIDER_EGRESS_READINESS_URL`. This is an operator
attestation—not automatic discovery—that the selected Spider deployment or
policy proxy rejects non-public initial destinations, DNS changes, and every
redirect target. The resolver explicitly requests Spider's strict redirect
policy and requires an authenticated capability endpoint on the exact normalized
`SPIDER_API_URL` origin. A separate readiness origin is not supported; deploy a
policy proxy by making it `SPIDER_API_URL` so it is also the actual fetch path.
The probe uses the Spider bearer credential, requires HTTPS, disables redirects,
caps the response at 4 KiB, and accepts only `application/json` with exactly:

```json
{
  "schema": "livy.resolver.spider-egress-capability/v1",
  "spider_api_url": "https://api.spider.cloud",
  "dns_all_answers_enforced": true,
  "redirect_every_hop_enforced": true
}
```

Unknown fields, a different upstream identity, false controls, plain 2xx
responses, and redirected probes fail closed. Loopback HTTP is available only
with the explicit `LIVY_RESOLVER_ALLOW_INSECURE_SPIDER_DEV=true` development
override. The resolver requires a successful probe before every debit/fetch.
Keep the policy at the infrastructure egress point where the actual connection
is made.

For a trusted internal deployment, the narrow
`LIVY_RESOLVER_TRUSTED_SOURCE_HOSTS` allow-list bypasses only the local
preflight. The broader `LIVY_RESOLVER_ALLOW_PRIVATE_SOURCES=true` override also
affects only local preflight. Neither bypasses the remote enforcement
attestation/readiness requirement, and neither should be used on an
internet-facing resolver without a separately reviewed egress policy.

`/healthz` is process liveness. `/readyz` performs the bounded Spider egress
capability probe and returns 200 only while it succeeds and the listener accepts
work. It switches to 503 before graceful shutdown drains requests and cancels
MCP work. Shutdown is hard-bounded by
`LIVY_RESOLVER_SHUTDOWN_GRACE_SECS` (10 seconds by default).

Errors retain the existing top-level `error` string and add stable `code` and
`request_id` fields. Responses include `x-request-id`; logs are JSON objects
and identify source URLs only by SHA-256. Raw bearer tokens, cookies, URLs,
query strings, and request bodies must not be logged.

Rate limiting is intentionally gateway-managed so limits remain consistent
across replicas. Apply burst limits by client IP or token hash at the ingress;
credit debits continue to provide tenant-level economic enforcement.

For operations, collect stderr with OpenTelemetry Collector, Vector, or Fluent
Bit. Prometheus and Grafana are suitable for request and gateway metrics, and
Sentry can aggregate Rust failures. Alert on 5xx rates, upstream latency and
timeouts, OAuth introspection failures, credit-service failures, validation
rejections, and gateway 429 responses. Do not attach raw source URLs as labels.

## MCP

- Endpoint: `/mcp`
- Protected resource metadata: `/.well-known/oauth-protected-resource` and `/.well-known/oauth-protected-resource/mcp`, including `resource_name` and the Livy OAuth introspection endpoint
- Server: `livy-resolver`
- Tool: `fetch_source` — input `{ "url": "...", "publish_provenance": false, "reveal_provenance_response": false }`
- Side effects: the descriptor truthfully marks the tool non-read-only and destructive because successful calls debit credits and store a tenant receipt; enabled provenance may create a private attestation, while public publication/reveal occurs only with authenticated explicit consent and deployment capability
- Output: successful calls include both `receipt_id` and `explorer`, where `explorer` is `https://explorer.livylabs.xyz/?q=<receipt_id>` with the actual receipt id substituted
- Auth: protected MCP requests require `Authorization: Bearer <livy_oauth_access_token>` with the exact `tool:fetch_source` scope, configured issuer, and resolver MCP endpoint audience. Authentication is performed once at the HTTP boundary and its context is reused by tool dispatch.
- Discovery: unauthenticated MCP requests return HTTP `401` with `WWW-Authenticate` pointing at the protected-resource metadata URL. After OAuth, clients can call `initialize`, `notifications/initialized`, and `tools/list` with the bearer token. The tool implementation keeps `_meta["mcp/www_authenticate"]` compatibility for contexts that reach tool dispatch directly.
- Transport: stateful sessions are disabled; requests use stateless JSON responses, avoiding unbounded in-memory sessions and replica stickiness. Requests are deadline-bound by `LIVY_RESOLVER_MCP_TIMEOUT_SECS`; deadline, disconnect, and shutdown cancellation propagate into the tool operation so upstream and side-effect work does not continue detached.
- Host/origin policy: `LIVY_RESOLVER_MCP_ALLOWED_HOSTS` and `LIVY_RESOLVER_MCP_ALLOWED_ORIGINS` are non-empty allow-lists. Defaults accept only local development values; production must explicitly set its public hostname and browser origins. Origins are matched by exact scheme, host, and effective port, so `https://app.example` permits port 443 but not port 4444.
- ChatGPT metadata: the `fetch_source` tool descriptor includes top-level `title` and `securitySchemes`, `_meta.securitySchemes`, short invocation status text, and open-world annotations. It is explicitly not read-only and is marked destructive because it can debit credits, store a receipt, and create irreversible public provenance.

Use when the prompt contains `source: <url>`, "only take this source", "source of truth", or an explicitly required URL. Pass the exact URL, don't search or substitute.

## Verify

```bash
curl -s http://localhost:3001/fetch \
  -H 'content-type: application/json' \
  -H "authorization: Bearer $LIVY_OAUTH_ACCESS_TOKEN" \
  -d '{"source":"https://example.com","mode":"fast","receipt":true}'
```

For a complete local gate, install the pinned tools listed in CI and run the
commands in [`CONTRIBUTING.md`](CONTRIBUTING.md). `scripts/smoke.sh` starts the
real binary, verifies liveness, fail-closed readiness, strict JSON rejection,
metrics, and SIGTERM drain. `scripts/verify-fresh-checkout.sh` proves that an
archived checkout builds and tests without a sibling dependency.

No repository license has been selected. This is a release blocker, and no
license field is claimed in Cargo metadata; see
[`docs/release-blockers.md`](docs/release-blockers.md).
