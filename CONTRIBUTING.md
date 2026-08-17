# Contributing

Use the pinned toolchain from `rust-toolchain.toml` and start from a clean
checkout. Do not create a sibling `livy-core`; the resolver owns its minimal
public provenance HTTP boundary locally.

Before opening a change, run:

```bash
cargo fmt --all -- --check
cargo check --locked --all-targets --all-features
cargo test --locked --all-targets --all-features
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo audit
cargo deny check advisories bans sources
cargo machete
ruby scripts/validate-contracts.rb
scripts/smoke.sh
```

Add a regression test for behavior changes. Boundary changes should prove the
HTTP status, side-effect order, tenant isolation, time/resource bound, and that
failure cannot create a debit, receipt, or provenance record. Keep logs and
metrics free of tokens, raw URLs, request bodies, tenant identifiers, and other
high-cardinality values.

Follow [`docs/dependency-policy.md`](docs/dependency-policy.md) for dependency
updates. Never commit `.env` or credentials. Do not add a LICENSE or Cargo
license field until the repository owner has made and documented that legal
choice.
