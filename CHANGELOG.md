# Changelog

All notable changes to indexkit are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.0.0] - 2026-04-23

### Added

- Public Rust library `indexkit` serving index constituent snapshots for
  the S&P 500 (IVV), S&P MidCap 400 (IJH), S&P SmallCap 600 (IJR),
  Nasdaq-100 (QQQ), and Dow Jones Industrial Average (DIA).
- Data sourced from SEC EDGAR N-PORT filings -- monthly, public-domain.
- Flat async API: `Indexkit::new().sp500(ym!(2024, 1)).await`.
- Free functions for one-off scripts: `indexkit::sp500_latest().await`.
- `YearMonth` type with `IntoYearMonth` trait -- accepts strings, u32,
  tuples, and an `ym!` macro for literals.
- Infallible `Indexkit::new()` + `try_new()` for early detection.
- Blocking wrappers (`*_blocking()`) for every async method.
- **Retry + exponential backoff**: up to 3 attempts, delays
  250 ms -> 750 ms -> 2 000 ms (capped). Retries on 5xx / 429 / connect /
  timeout. Honours `Retry-After` response header.
- **Single-flight per-key cache**: concurrent callers requesting the same
  month share one HTTP fetch via `Arc<OnceCell>` deduplication.
- **CDN mirror fallback**: after primary URL retries are exhausted, the
  fetcher tries jsDelivr (`cdn.jsdelivr.net/gh/userFRM/indexkit@main/data`).
  Override with `INDEXKIT_MIRROR_URL` or `with_mirror_url(...)`.
- **SHA-256 manifest verification**: `data/manifest.json` maps each parquet
  filename to its expected `sha256:<hex>` digest. Downloaded bytes are
  verified before being written to the local cache. New
  `Error::ChecksumMismatch` variant. CLI sub-command `indexkit-cli manifest`
  regenerates the manifest from local `data/`.
- **SEC N-PORT parser**: streaming `quick-xml` parser for the
  `primary_doc.xml` schema. Extracts holdings (name, CUSIP, LEI, shares,
  market value, weight, asset category) and filters by series ID for
  multi-series trusts.
- **CIK / series map verified against live SEC**: all five ticker -> CIK ->
  series pairs were confirmed by fetching real N-PORT filings during v1.0
  build-out. See `data/cik-map.json`.
- CLI commands: `backfill`, `nightly-append`, `get`, `manifest`, `cik-map`.
- GitHub Actions workflows: `ci.yml`, `backfill.yml` (manual),
  `nightly.yml` (cron `0 7 * * 1-5`), `release.yml` (on `v*` tags).
- Docs: `README.md` (yfd-style), rustdoc with doctests,
  `docs/api.md`, `docs/architecture.md`, `docs/data-sources.md`.

### Known limitations

- **No ticker symbols**: N-PORT does not include tickers. Every
  `Constituent::ticker` is `None`. Use CUSIP as the primary join key and
  enrich via OpenFIGI or a CUSIP -> ticker map downstream. The
  `tickers(...)` and `sp500_tickers_latest()` methods return empty vectors
  until a ticker source is added.
- **No GICS sector**: deferred to v1.1. `Constituent::sector` is always
  `None` in v1.0.
- **No issuer CIK**: N-PORT does not include issuer CIK in holdings
  records. `Constituent::issuer_cik` is always `None` in v1.0.
- **60-90 day filing lag**: ETFs must file N-PORT ~60 days after each
  reporting period end, and the SEC delays public release another 30 days.
  The "latest" snapshot is typically two to three months old.
- **Coverage starts 2019-11**: SEC public N-PORT filing began Q4 2019.

### Deviations from initial brief

- `DIA`'s trust CIK in the initial brief was `0000816853`; live SEC lookup
  showed that CIK does not exist. Corrected to `0001041130` (SPDR Dow Jones
  Industrial Average ETF Trust). DIA is a single-series trust, so
  `series_id` is `None`.
- `IJR`'s series_id in the initial brief was `S000004315`; the correct
  value (confirmed against a 2023-12-31 IJR filing) is `S000004313`.
- IVV's trust CIK in the initial brief was `0000921669` (which turned out
  to be Carl Icahn's personal CIK). The correct iShares Trust CIK is
  `0001100663`.
- QQQ series_id was `null` in the initial brief; the correct value is
  `S000101292`.

[1.0.0]: https://github.com/userFRM/indexkit/releases/tag/v1.0.0
