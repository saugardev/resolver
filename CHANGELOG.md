# Changelog

All notable changes to this project are documented here. The project has not
made a stable public release.

## Unreleased

### Security

- Default provenance to private, explicit, consent-gated behavior.
- Scope receipts to tenant/project with random identifiers, expiry, and bounds.
- Fail closed on unsafe source destinations and unverified remote egress.
- Enforce one OAuth boundary, bounded stateless MCP, and truthful tool effects.
- Reject every unsuccessful, redirected, oversized, or timed-out upstream
  result before credit capture or evidence finalization.

### Changed

- Rename the package and MCP server from the stale `livygensyn` identity to
  `livy-resolver`.
- Make request JSON schemas reject unknown fields.
- Remove the experimental attestation file, inert feature, demo receipt field,
  and misspelled `/recipt/{id}` route.
- Replace the private sibling provenance SDK path with a resolver-owned HTTP
  client built from the public backend contract.

### Operations

- Add pinned CI, dependency and secret policy gates, a non-root container,
  health/readiness/drain behavior, low-cardinality metrics, smoke tests, an
  OpenAPI contract, an MCP client example, and an operations runbook.
