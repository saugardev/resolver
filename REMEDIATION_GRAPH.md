# Audit Remediation Graph

This branch is the integration baseline for the findings recorded in
`audit-report-2026-08-17.html` and `.superstack/build-context.md`.

## Control loop

```text
master
  -> worker (isolated worktree and fork branch)
  -> fresh reviewer (read-only review and verification)
  -> master decision
       -> accept and integrate
       -> or return concrete findings to the worker and repeat with a new reviewer
```

The master owns integration, conflict resolution, acceptance, and the final audit.
Workers must keep changes inside their assigned branch, add tests, run the relevant
checks, and commit their work. Reviewers do not edit implementation branches.

## Related issue groups

### 1. Data boundaries and billing

- Branch: `saugardev/fix-data-boundaries`
- Workspace: `data-boundaries`
- Findings: C-01, C-02, M-01, M-02, and the data/publication part of M-08.
- Required outcome:
  - provenance is explicitly enabled and defaults to private/non-publishing;
  - compatibility and snapshot paths preserve the authenticated caller context;
  - receipts use unpredictable identifiers, are tenant-owned, expire, and have a
    bounded store with a production-safe storage abstraction;
  - product-credit idempotency is caller-controlled and enforcement outcomes are
    honored before work executes;
  - tests prove cross-tenant receipt access is denied and unsafe provenance defaults
    cannot silently activate.

### 2. Fetch correctness and resource bounds

- Branch: `saugardev/fix-fetch-runtime`
- Workspace: `fetch-runtime`
- Findings: H-01, upstream-response portions of H-04, M-03, M-04, and M-05.
- Required outcome:
  - every Spider HTTP non-success response becomes an upstream error and cannot be
    returned or billed as a successful fetch;
  - upstream calls have explicit deadlines and response-size limits;
  - adaptive fallback can affect the current request where safe and preserves error
    classification;
  - fetch modes and proxy parameters have one validated, documented contract;
  - diagnostic body compaction is Unicode-safe;
  - regression tests cover non-2xx responses, timeouts/limits, fallback, proxy
    conflicts, and multibyte bodies.

### 3. MCP, authentication, and network hardening

- Branch: `saugardev/fix-mcp-security`
- Workspace: `mcp-security`
- Findings: H-02, H-03, MCP/session portions of H-04, M-06, M-08, and runtime
  portions of M-09.
- Required outcome:
  - MCP host/origin validation is enabled and configurable with secure defaults;
  - source egress policy blocks loopback, private, link-local, and metadata targets
    by default, including DNS resolution checks, with an explicit trusted override;
  - authentication is performed once per request with consistent issuer, audience,
    and scope enforcement;
  - MCP sessions and relevant network calls have idle limits, caps, and deadlines;
  - tool annotations reflect credit, receipt, and provenance side effects;
  - readiness and graceful shutdown distinguish liveness from dependency health;
  - security and lifecycle tests exercise the boundaries.

### 4. Build, CI, tests, and release quality

- Branch: `saugardev/fix-release-quality`
- Workspace: `release-quality`
- Findings: H-05, H-06, M-07, remaining M-09, L-01, and L-02.
- Dependency: starts after groups 1-3 are accepted and integrated.
- Required outcome:
  - a fresh checkout has a reproducible dependency layout or documented bootstrap;
  - CI gates formatting, compilation, tests, strict Clippy, dependency advisories,
    and secret scanning;
  - vulnerable lockfile entries are removed or explicitly proven unreachable and
    policy-documented;
  - boundary/integration/concurrency tests cover the integrated application;
  - dead or experimental code and misleading feature flags are removed or finished;
  - deployment, MCP client configuration, API contract, license, toolchain, and
    operational documentation are present and consistent.

## Acceptance rule

A branch is accepted only when a fresh reviewer reports no unresolved critical or
high-severity regression, required checks pass, and the master confirms the change
meets this file's required outcome. Any remaining infrastructure-dependent item must
be represented by a safe default, an explicit runtime failure/readiness signal, and a
tracked production requirement rather than a silent fallback.
