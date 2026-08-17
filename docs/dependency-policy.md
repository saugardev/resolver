# Dependency and supply-chain policy

## Resolution

`Cargo.lock` is committed and every CI build uses `--locked`. Direct registry
dependencies use explicit compatible versions; wildcard and unknown registry
or Git sources are denied. New Git or external path dependencies are not
accepted: publish the crate, implement the small public protocol boundary in
this repository, or make it a repository-owned workspace member.

The resolver's provenance HTTP client is implemented locally from the public
resolver/backend contract. It replaces the former developer-only private
`../livy-core` dependency without copying or publishing private repository
source. Changes to request paths or schemas require a public contract fixture
and a boundary test.

## Gates

CI runs:

- locked all-target/all-feature compilation and tests;
- formatting and Clippy with every warning denied;
- `cargo audit` with no ignored advisories;
- `cargo deny check advisories bans sources`;
- `cargo machete` for unused direct dependencies;
- Gitleaks against complete Git history;
- a build/test from an archived checkout with no sibling repository;
- a live service smoke and a production container build.

Tool versions, the Rust toolchain, GitHub Actions, downloaded scanner checksum,
and container base images are pinned. Dependabot opens weekly Cargo, Actions,
and Docker updates; every update must pass the same gates.

## License inventory blocker

Cargo license enforcement is intentionally not represented as passing. This
repository has no authoritative license from the owner. Inventing an SPDX value
or treating a notice as a license would create false assurance. Public
distribution is blocked until the owner selects and adds a repository license.
Once that happens, enable `cargo deny check licenses` with an explicit
allow-list and no undocumented exceptions.
