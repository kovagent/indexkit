# Changelog

All notable changes to indexkit are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `parse_spdr_xlsx` parser for State Street SPDR daily-holdings XLSX
  (DIA / MDY / SPY etc.) using `calamine`. Strips the preamble,
  locates the `Ticker` header dynamically, filters cash and sub-total
  rows, and parses the `As of MMM DD, YYYY` stamp from the preamble.
  Three unit tests against a committed real-DIA fixture
  (`crates/indexkit/tests/fixtures/spdr_dia_sample.xlsx`).
- `IndexId::Rut` (Russell 2000) wired through `IndexId::ALL`,
  `from_str_id`, `as_str`, `cik::entry_for` (iShares Trust CIK
  0001100663, series S000004361) and `sponsor_url` (IWM via the
  iShares CSV CDN).
- `sponsor::sponsor_urls(IndexId) -> Vec<(DataSource, &'static str,
  &'static str)>` returns AUM-ranked endpoints (primary first,
  backups follow). Per-index ladder:
  - SP500: SPY (SSGA SPDR XLSX) > IVV (iShares CSV)
  - SP400: IJH (iShares CSV) > MDY (SSGA SPDR XLSX)
  - SP600: IJR (iShares CSV) > SLY (SSGA SPDR XLSX)
  - NDX: QQQ (Invesco JSON) > QQQM (Invesco JSON)
  - DJIA: DIA only
  - RUT: IWM only
- `SponsorClient::fetch_today` now walks `sponsor_urls` and falls
  back to each backup endpoint on 4xx / 5xx / network failure,
  warn-logging the failed primary. Returns the source tag and bytes
  of the first successful endpoint.

### Changed

- `cli::cmd_daily_fetch` now invokes `parse_spdr_xlsx` for
  `DataSource::SpdrCdn` instead of returning the
  `"SPDR XLSX not parseable in v1.0"` error. DIA daily holdings are
  fetched on every nightly run going forward.
- `cli::cmd_wayback_backfill` now iterates `sponsor_urls` per index
  (not just the primary), unifying CDX + snapshot ingest across
  primary and backup endpoints. Same parser dispatch (iShares CSV /
  Invesco JSON / SPDR XLSX) per source tag.
- **SP500 primary flipped from IVV to SPY.** SPY (~$650 B AUM,
  SSGA SPDR XLSX) is larger than IVV (~$600 B, iShares CSV) and
  uses the same parser path as DIA. IVV remains as the secondary
  endpoint. VOO (Vanguard, ~$700 B) outranks both by AUM but is
  deferred until a stable scraper for Vanguard's JS-rendered
  holdings page exists.
- `sponsor::sponsor_url` retained as a backwards-compatible thin
  wrapper that returns the first (primary) entry of `sponsor_urls`.

## [1.0.1] - 2026-04-24

### Added

- **GitHub mirror sources**: three free OSS ingestion paths for
  historical index constituent data, combining to provide daily S&P 500
  coverage from 1996-01-02 onward (up from quarterly 2019-11 via N-PORT
  only).
  - `DataSource::GithubFja05680` -- fja05680/sp500 (MIT), S&P 500 daily
    change-rows 1996 -> present.
  - `DataSource::GithubYfiua { month }` -- yfiua/index-constituents
    (Apache-2.0), S&P 500 / Nasdaq-100 / Dow Jones monthly snapshots
    ~2018 -> present.
  - `DataSource::GithubHanshof` -- hanshof/sp500_constituents (MIT),
    S&P 500 daily change-rows 1996 -> present (cross-check layer).
- `github_mirror` module -- public async fetchers
  (`fetch_fja05680_sp500`, `fetch_hanshof_sp500`, `fetch_yfiua`,
  `fetch_yfiua_full`), CSV parsers, and `forward_fill` helper for
  expanding change-row data into per-calendar-day rows.
- `Constituent::weight_opt() -> Option<f64>` -- returns `None` when
  `weight` is `NaN` (the sentinel used by ticker-only GitHub mirror
  sources) or non-finite.
- `IndexSnapshot::has_weights() -> bool` -- quick gate for "is this a
  weight vector or just a ticker universe".
- CLI `github-backfill [--source fja05680|yfiua|hanshof]` command --
  ingests the three OSS mirrors. Logs cross-source disagreements
  between fja05680 and hanshof per date.
- New GitHub Actions workflow `.github/workflows/oss-backfill.yml` to
  run `github-backfill` on `workflow_dispatch`.
- `data/licenses/` directory -- ships verbatim upstream LICENSE files
  for the three OSS mirrors (MIT, Apache-2.0, MIT).

### Changed

- **Coalesce priority** widened to six tiers: sponsor CDN (5) >
  GithubFja05680 (4) > GithubYfiua / GithubHanshof (3) > Wayback (2) >
  SecNport (1). See `DataSource::priority` rustdoc.
- **Coalesce identity key** now prefers CUSIP (for CDN / Wayback /
  N-PORT rows) and falls back to ticker (for GitHub mirror rows with
  empty CUSIP). Falls back further to name if neither is available.
  This keeps existing CUSIP-bearing dedup behaviour unchanged.
- Data coverage: S&P 500 now has daily rows from 1996-01-02 -> present
  (via GithubFja05680 / GithubHanshof), up from quarterly 2019-11 ->
  present in v1.0.0.
- `README.md` + `docs/data-sources.md` rewritten to document all six
  source tiers with license + attribution.

### Notes on API shape

- No breaking change to any public type or method signature. The
  `Constituent` struct preserves its v1.0.0 shape. Ticker-only rows
  use `f64::NAN` in the `weight` field as a sentinel and empty
  strings for `cusip`. Prefer the new `weight_opt()` accessor over
  direct access when consuming mixed-source data.
- The `DataSource` enum gained three new variants. This is a minor
  bump in strict semver terms but is released as v1.0.1 per repository
  convention (v1.0.0 made no stability pledge around enum
  exhaustiveness; no consumers on crates.io match on `DataSource`
  non-exhaustively at the time of release).

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
[1.0.1]: https://github.com/userFRM/indexkit/releases/tag/v1.0.1
