# Audit Remediation Graph

This file records the completed worker → fresh reviewer → master loop for the
findings in `audit-report-2026-08-17.html`. The integrated fork branch is
`saugardev/audit-remediation`; the accepted code tip before this final report is
`472f70d4945b92312a5170e12a8bfa2167e2c9b9`.

## Control loop

```text
master
  ├─ groups related findings on one fork branch/workspace
  ├─ sends context and acceptance criteria to a worker
  ├─ boots a new read-only reviewer after each worker iteration
  └─ judges the report
       ├─ CHANGES_REQUIRED → same worker → new commit → new reviewer
       └─ ACCEPT → integration branch → integration reviewer → master branch
```

Workers owned implementation branches. Reviewers did not edit them. The master
owned grouping, acceptance, integration, GitHub issue state, and the final audit.

## Completion ledger

| Group | Fork branch | Accepted tip | Final reviewer | Result |
|-------|-------------|--------------|----------------|--------|
| Data boundaries, provenance, receipts, billing | `saugardev/fix-data-boundaries` | `03dcd7c` | `data_boundaries_reviewer_4` | ACCEPT |
| Fetch correctness and resource bounds | `saugardev/fix-fetch-runtime` | `6300c74` | `fetch_runtime_reviewer_3` | ACCEPT |
| MCP, authentication, egress, lifecycle | `saugardev/fix-mcp-security` | `d962563` | `mcp_security_reviewer_4` | ACCEPT |
| Combined source integration | `saugardev/source-integration` | `4ab7d90` | `source_integration_reviewer_1` | ACCEPT |
| Build, CI, tests, contracts, release quality | `saugardev/fix-release-quality` | `472f70d` | `release_quality_reviewer_4` | ACCEPT |

The release-quality branch passed fork CI run
[`32079887704`](https://github.com/saugardev/resolver/actions/runs/32079887704)
at its exact tip. Both the Rust/policy/secrets/smoke job and hardened
production-container job were green.

## Implemented outcomes

### 1. Data boundaries and billing

- Provenance requires explicit activation and defaults to private,
  non-publishing behavior.
- Public provenance fields contain SHA-256 commitments; publication and reveal
  require authenticated request-level consent.
- Receipts use UUIDs, tenant/project ownership, TTL/capacity, and an async public
  storage/factory interface with health checks.
- The built-in memory store is rejected unless an explicit single-replica
  exception is acknowledged.
- Credit keys include the authenticated subject and canonical validated
  execution plan. Unenforced, mismatched, generic-ledger, and cross-subject
  outcomes cannot finalize evidence.
- Snapshot and compatibility paths produce evidence from the exact executed plan.

### 2. Fetch correctness and resource bounds

- Every Spider operation uses one application-owned, status-aware transport.
- Redirect following is disabled; non-success statuses are rejected.
- One absolute deadline and streamed response cap cover headers and body reads.
- Adaptive fallback may run once for enumerated challenge statuses under the same
  deadline; redirects and 5xx bodies cannot become fallback success.
- Billing order is authenticated preflight → validated pending fetch → enforced,
  request-bound capture → receipt/provenance finalization.
- Mode/route combinations are validated, deprecated proxy fields are gone, and
  diagnostic compaction is Unicode-safe.

### 3. MCP, authentication, egress, and lifecycle

- RMCP Host and Origin allow-lists are enabled with non-empty secure defaults.
- OAuth validates issuer, explicit audience, exact scope, and subject once; the
  resulting context is reused by tool dispatch.
- Stateless RMCP plus request cancellation, bounded drain, and deadlines prevent
  detached work after timeout.
- Local source validation rejects sensitive hostnames, mixed/empty DNS answers,
  private/special-use addresses, and both IPv4-mapped/translated IPv6 layouts.
- Remote fetch is fail-closed unless an authenticated, same-origin, bounded
  capability document attests redirect and DNS enforcement; readiness probes it.
- Tool descriptors truthfully report billing/storage/publication side effects.
- Liveness, readiness, graceful shutdown, and metrics have runtime tests.

### 4. Release quality

- The repository is self-contained; it has no sibling path, Git, private SDK, or
  private-source dependency.
- Rust `1.94.0`, the lockfile, workflow actions, audit tools, and container bases
  are pinned or policy-controlled.
- Fork CI gates format, locked all-target/all-feature check and 127 tests, strict
  Clippy, contracts, ShellCheck, RustSec, cargo-deny, cargo-machete, Gitleaks full
  history, isolated fresh checkout, service smoke, and a hardened non-root
  container build/runtime smoke.
- API schemas reject unknown fields; OpenAPI, MCP client example, operations,
  dependency, security, contribution, and changelog documentation are present.
- Experimental/demo artifacts and misleading package/feature identity were removed.

## External release blockers

Repository remediation is accepted, but public or multi-replica production release
remains blocked until all four are closed:

1. The owner selects an authoritative repository license and matching Cargo metadata.
2. Deployment supplies a durable, shared, tenant-scoped receipt adapter.
3. The Livy backend supplies atomic reserve/capture/cancel and authoritative replay.
4. The remote Spider/policy deployment enforces DNS and every redirect hop and
   exposes the authenticated readiness capability.

These are also tracked in `docs/release-blockers.md`. Safe defaults and readiness
signals prevent them from silently degrading into an accepted production posture.

## Acceptance rule

A branch was accepted only after a new reviewer reported no unresolved required
repo-side blocker, proportionate checks passed, and the master independently judged
the acceptance criteria. Remaining infrastructure-dependent items are explicit
release blockers; they are not counted as silently completed work.
