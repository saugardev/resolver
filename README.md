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

Request fields include `source`, `query`/`q`, `mode`, `format`, `proxy`, and
`receipt`. The mode contract is route-specific:

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
registry plus the durable debit ledger; cross-process caller-key races and an
ambiguous capture without a caller key remain residual limitations.

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

Prefer `/fetch` with `mode` over the compat routes.

## API security

Product requests are limited to 64 KiB and 65 seconds by default. Override
these deployment safety limits with:

```dotenv
LIVY_RESOLVER_MAX_PRODUCT_BODY_BYTES=65536
LIVY_RESOLVER_PRODUCT_TIMEOUT_SECS=65
LIVY_RESOLVER_MAX_UPSTREAM_BYTES=8388608
LIVY_RESOLVER_HSTS_ENABLED=false
```

Spider calls have a five-second connect timeout, a 65-second client ceiling,
the request's `timeout_secs` absolute deadline (1–60 seconds), and the response
limit above. Redirect following is disabled, so control-plane 3xx responses are
errors even when their target would return 2xx. Every non-2xx response and every
payload item whose source `status` is outside 200–299 maps to an upstream error
before credit capture, receipt creation, or provenance creation.

Enable HSTS only when the public endpoint is served through HTTPS. The API
accepts absolute HTTP and HTTPS source URLs, including localhost and private
addresses, because internal-source resolution is supported. Deploy Spider and
the resolver behind egress controls that block cloud metadata services and
other destinations that must not be reachable.

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
- Auth: protected MCP requests require `Authorization: Bearer <livy_oauth_access_token>` with `tool:fetch_source` or `mcp` scope and the resolver MCP endpoint audience
- Discovery: unauthenticated MCP requests return HTTP `401` with `WWW-Authenticate` pointing at the protected-resource metadata URL. After OAuth, clients can call `initialize`, `notifications/initialized`, and `tools/list` with the bearer token. The tool implementation keeps `_meta["mcp/www_authenticate"]` compatibility for contexts that reach tool dispatch directly.
- ChatGPT metadata: the `fetch_source` tool descriptor includes top-level `title` and `securitySchemes`, `_meta.securitySchemes`, short invocation status text, and read-only/open-world annotations

Use when the prompt contains `source: <url>`, "only take this source", "source of truth", or an explicitly required URL. Pass the exact URL, don't search or substitute.

## Verify

```bash
curl -s http://localhost:3001/fetch \
  -H 'content-type: application/json' \
  -H "authorization: Bearer $LIVY_OAUTH_ACCESS_TOKEN" \
  -d '{"source":"https://example.com","mode":"fast","receipt":true}'
```
