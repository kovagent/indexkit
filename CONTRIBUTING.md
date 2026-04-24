# Contributing to indexkit

Thank you for your interest in indexkit!

## Local setup

```bash
git clone https://github.com/userFRM/indexkit
cd indexkit
cargo build --workspace
cargo test --workspace
```

Rust stable (1.77+) is required. No system libraries are needed -- all
dependencies are pure Rust or bundled.

## Before submitting a PR

Run the full local CI check:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test --doc
```

All four must pass. Live-network tests (fetching from SEC EDGAR / GitHub
raw) are gated behind `#[ignore]` and are not required for CI.

## Pull request conventions

- One logical change per PR.
- Include a one-sentence "why" in the PR description.
- Update `CHANGELOG.md` under `[Unreleased]`.
- No external API keys -- all data sources are public-domain.

## Data source changes

If you change a CIK or series ID in `src/cik.rs`, verify against live SEC
data first:

```bash
curl -sS -H "User-Agent: indexkit/1.0 (+https://github.com/userFRM/indexkit)" \
  "https://data.sec.gov/submissions/CIK0001100663.json" | jq .name
```

Then update `docs/data-sources.md` and add the verification output to the PR
description.

If you change the parquet schema (`data/{index}/*.parquet`), update
`docs/architecture.md` and `src/parquet_io.rs` accordingly. Bump the minor
version and document the schema change in `CHANGELOG.md`.

## License

By contributing, you agree your contributions will be licensed under the
Apache-2.0 License.
