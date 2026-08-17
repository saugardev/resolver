# Livy Web Resolver

Default mode is SmartMode. Override via `mode` when you need specific cost/speed/browser/unblock behavior.

## Status

- [x] Browserful query and browserless URL resolver
- [x] Sanitized markdown and request params
- [x] Proxy enabling for complex requests
- [ ] Residential proxy rotations
- [ ] Prompt-based interaction with results
- [x] MCP auth
- [ ] Obscura headless (research)

## Setup

```
LIVY_RESOLVER_KEY=your-resolver-key
cargo run
```

`SPIDER_API_KEY`, `SPIDER_KEY`, and `LIVY_KEY` remain accepted as legacy
aliases for `LIVY_RESOLVER_KEY`.
The service listens on `http://localhost:3001` unless `PORT` or
`RESOLVER_PORT` is set.

Copy `.env.example` to `.env` for the full local configuration template.

Product routes and MCP requests require a Livy OAuth bearer token by default.
Unauthenticated MCP requests, including `initialize` and `tools/list`, return
HTTP `401` with `WWW-Authenticate` so Claude can discover the OAuth protected
resource and show the sign-in flow. Local-only unauthenticated development can
set `LIVY_RESOLVER_AUTH_ENABLED=false`.

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
- `schema_id=resolver-fetch-v1`
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
LIVY_PROVENANCE_VISIBILITY=public
LIVY_PROVENANCE_VERIFICATION_MODE=verify_fresh
LIVY_EXPLORER_BASE_URL=https://api.livylabs.xyz
LIVY_PROVENANCE_BOOTSTRAP_TEMPLATE=false
LIVY_PROVENANCE_MANAGED_PUBLICATION=true
LIVY_PROVENANCE_PUBLISH_RESPONSE_ARTIFACT=true
LIVY_PROVENANCE_RESPONSE_ARTIFACT_MAX_BYTES=262144
LIVY_PROVENANCE_WAIT_FOR_REGISTRY_REFS=false
LIVY_PROVENANCE_REGISTRY_WAIT_ATTEMPTS=30
LIVY_PROVENANCE_REGISTRY_WAIT_INTERVAL_MS=2000
```

`LIVY_BACKEND_BASE_URL` defaults to `https://api.livylabs.xyz`; set it for local or staging backends.

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
commitment-only. Response artifacts default on for public provenance and off
for private provenance. Managed publication is public and irreversible, so
enable it only for resolver outputs that are safe to disclose. The resolver
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

Request fields: `source`, `query`/`q`, `mode` (`auto|fast|browser|unblock|raw|crawl|map|search|extract|screenshot`), `format`, `proxy`, `receipt`.

| Method | Path | Purpose |
|---|---|---|
| POST | `/fetch` | Fetch one URL |
| POST | `/crawl` | Crawl with `limit`, `depth` |
| POST | `/map` | Discover links |
| POST | `/search` | Web search (+ optional page fetch) |
| POST | `/extract` | Extraction with selectors |
| POST | `/screenshot` | Capture screenshot |
| POST | `/fetchfast` | Compat: fast fetch |
| POST | `/fetchunblock` | Compat: unblock fetch |
| GET | `/receipt/{id}` | Read receipt |

Prefer `/fetch` with `mode` over the compat routes.

## API security

Product requests are limited to 64 KiB and 65 seconds by default. Override
these deployment safety limits with:

```dotenv
LIVY_RESOLVER_MAX_PRODUCT_BODY_BYTES=65536
LIVY_RESOLVER_PRODUCT_TIMEOUT_SECS=65
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

For operations, collect stdout with OpenTelemetry Collector, Vector, or Fluent
Bit. Prometheus and Grafana are suitable for request and gateway metrics, and
Sentry can aggregate Rust failures. Alert on 5xx rates, upstream latency and
timeouts, OAuth introspection failures, credit-service failures, validation
rejections, and gateway 429 responses. Do not attach raw source URLs as labels.

## MCP

- Endpoint: `/mcp`
- Protected resource metadata: `/.well-known/oauth-protected-resource` and `/.well-known/oauth-protected-resource/mcp`, including `resource_name` and the Livy OAuth introspection endpoint
- Server: `livygensyn-source-fetcher`
- Tool: `fetch_source` — input `{ "url": "..." }`
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
