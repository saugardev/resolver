# Security policy

## Supported version

Only the latest commit on the maintained default branch is supported before a
first stable release. Security fixes are not promised for older `0.1.x`
snapshots.

## Report a vulnerability

Use the private GitHub Security Advisory form for the upstream repository:
<https://github.com/livylabs/resolver/security/advisories/new>.

Do not include live credentials, bearer tokens, private source URLs, tenant
data, or exploit traffic in a public issue. Include the affected commit,
impact, safe reproduction steps, and whether a debit, receipt, OAuth boundary,
SSRF control, or provenance disclosure is involved. If the advisory form is
unavailable, ask a repository maintainer for a private channel without posting
the vulnerability details publicly.

## Security-sensitive changes

Changes to authentication, tenant ownership, credits, source egress,
provenance disclosure, dependency sources, CI workflows, or receipt storage
require tests at the service boundary and review by a maintainer who did not
author the change. Never weaken a fail-closed default to make a deployment
probe pass.
