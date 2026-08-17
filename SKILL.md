---
name: livy-resolver
description: |
  Use this skill when configuring or working with the Livy Resolver MCP
  service in this repo. It explains how the resolver relates to MCP
  clients, where Spider and Livy provenance keys must be configured, and
  how to use the exact-source `fetch_source` tool and product HTTP
  routes.
---

# Livy Resolver

Livy Resolver is this repo's Axum + RMCP service for resolving web
sources through Spider-backed Livy access. It exposes:

- an MCP endpoint at `http://localhost:3001/mcp`
- one MCP tool, `fetch_source`, for exact source-of-truth URL fetching
- HTTP routes for product-style fetch, crawl, map, search, extract,
  screenshot, unblock, and receipt flows

## Required Configuration

The resolver requires:

```dotenv
LIVY_RESOLVER_KEY=your-resolver-key
```

This repo does not include an MCP config file yet. The code loads `.env`
and then reads `LIVY_RESOLVER_KEY` from the resolver process environment at
startup. `SPIDER_API_KEY`, `SPIDER_KEY`, and `LIVY_KEY` are accepted as legacy
aliases.

Configure the key based on how MCP is wired:

- If the MCP config launches this repo, put `LIVY_RESOLVER_KEY` in that
  server entry's `env`.
- If the MCP client connects to an already-running
  `http://localhost:3001/mcp`, configure `LIVY_RESOLVER_KEY` where that
  server process is started.
- For local manual runs, use a shell env var or local `.env`.

Example launch config:

```json
{
  "mcpServers": {
    "livy-resolver": {
      "command": "cargo",
      "args": ["run"],
      "cwd": "/path/to/resolver",
      "env": {
        "LIVY_RESOLVER_KEY": "your-resolver-key"
      }
    }
  }
}
```

## Livy Provenance

When enabled, successful resolver fetches also post Livy provenance
attestations to the backend. The current proof is generic source-fetch
provenance:

- `attestation_claim=source`
- `subject_type=resolver_fetch`
- `schema_id=resolver-fetch-v1`, `schema_version=1` (current backend contract)
- the public `source` value is a stable `sha256:<digest>` commitment, never the
  raw URL or query
- `integration_id=delphi` by default

Do not use the `prediction_market_resolver` template unless the resolver
is actually producing market-resolution outputs such as market id,
outcome, confidence, dispute window, and settlement target.

Configure provenance where the resolver process runs:

```dotenv
LIVY_PROVENANCE_ENABLED=true
LIVY_BACKEND_BASE_URL=https://api.livylabs.xyz
LIVY_INTEGRATION_ID=delphi
ITA_API_KEY=...
```

For OAuth-protected MCP `fetch_source`, the resolver posts provenance with
the user's Livy OAuth bearer token to
`/api/v1/resolver/source-fetch-attestations`; tenant/project come from the
token claims. Do not grant ChatGPT clients generic
`provenance:attestation:write`.

Optional:

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
```

`LIVY_API_KEY` is for legacy/local service-key provenance writes. Only set
`LIVY_PROVENANCE_BOOTSTRAP_TEMPLATE=true` if that API key has template
write scope. Public explorer reads require the matching public template to
exist in Livy.

Managed publication uploads both `provenance-receipt.json` and a versioned
request/response exchange as `resolver-response.json`. The
`resolver-tool-exchange-v1` payload exposes the sanitized committed request as
`request`, the exact upstream JSON as `response`, and both SHA-256 commitments.
Header and cookie values remain redacted. Oversized exchanges stay
commitment-only, and `LIVY_PROVENANCE_PUBLISH_RESPONSE_ARTIFACT=false` disables
response reveals. Response reveals and managed publication default off, while
the source URL is committed rather than public. Public visibility or managed
publication also requires
`LIVY_PROVENANCE_ALLOW_PUBLIC_DISCLOSURE=true`. Arweave publication is public
and irreversible. These deployment settings are capability gates only:
authenticated callers must also send explicit per-request publication consent,
and separate reveal consent is required for the exact upstream response. Both
default false. The resolver rejects non-v1 schema configuration at startup; a
future v2 requires an external backend contract rollout first.

## MCP Use

OAuth is enabled by default. Unauthenticated MCP requests, including
`initialize`, `notifications/initialized`, and `tools/list`, return HTTP `401`
with `WWW-Authenticate` pointing at `/.well-known/oauth-protected-resource`.
The protected-resource metadata `resource` must match the public MCP URL,
such as `https://resolver.api.livylabs.xyz/mcp`, because Claude matches the
registered connector URL exactly during OAuth discovery.
Set `LIVY_RESOLVER_AUTH_ENABLED=false` only for local unauthenticated
development, and also set `LIVY_RESOLVER_CREDITS_ENABLED=false`; enabled credit
enforcement fails closed without an authenticated access token. Authenticated
credit enforcement also requires a non-empty introspection `sub`; caller
idempotency is isolated by that OAuth subject as well as tenant, project,
client, and request fingerprint. A locally finalized key returns a conflict
before Spider; generic ledger rows are not trusted to authorize replay after a
restart.

Use `fetch_source` when the user gives an exact URL as the required
source:

```json
{
  "url": "https://example.com/article",
  "publish_provenance": false,
  "reveal_provenance_response": false
}
```

Do not search first, replace the URL, or infer a different source. Pass
the exact URL and use the returned content plus receipt metadata.
The tool is intentionally described as non-read-only/destructive: a successful
call debits credits and stores a tenant receipt, and may publish provenance only
when both authenticated request consent and deployment capability allow it.
Enabled provenance can create a private attestation without public publication
consent; publication and response reveal are the separately consented effects.

## Local Run

```bash
cargo run
```

Server URL:

```text
http://localhost:3001
```

MCP URL:

```text
http://localhost:3001/mcp
```

## HTTP Routes

Use these for product/API clients:

- `POST /fetch`
- `POST /crawl`
- `POST /map`
- `POST /search`
- `POST /extract`
- `POST /screenshot`
- `POST /fetchfast`
- `POST /fetchunblock`
- `GET /receipt/{id}`

Example:

```bash
curl -s http://localhost:3001/fetch \
  -H 'content-type: application/json' \
  -d '{"source":"https://example.com","mode":"fast","receipt":true}'
```

## Code Map

- `src/main.rs`: mounts HTTP routes and `/mcp`
- `src/mcp.rs`: defines `fetch_source`
- `src/fetch.rs`: reads `LIVY_RESOLVER_KEY`, calls Spider, stores receipts
- `src/provenance.rs`: builds and posts generic resolver source-fetch
  attestations
- `src/api.rs`: HTTP route handlers
- `src/types.rs`: request/response types

After changes, run:

```bash
cargo check
```
