# Changelog

All notable changes to indexkit are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

This release fixes every open issue and changes the public API, so it is a
major version. The upgrade notes come first.

### Upgrading from 1.0

- `IndexId` and `DataSource` are `#[non_exhaustive]` and have new variants
  (`IndexId::Rut`, `DataSource::NasdaqApi`), so a `match` on either needs a
  wildcard arm. `IndexId::ALL` has six entries.
- `sp500_latest`, `ndx_latest`, `dji_latest` and the `*_latest` client
  methods return the newest day only, where they returned every row of the
  newest month. `latest(id)` returns that day with its date and source.
- `on` and `daily_range` answer each day from one source. On a quarter-end
  day that has both the fund's N-PORT filing and the membership list, the
  list answers: it is complete where the filing is not, but it has no
  weights. The filing's weights stay available for that month through
  `constituents` and `weight`.
- `sponsor_url` returns each index's current primary, which for the S&P 500
  is now SPY's XLSX file and for the Nasdaq-100 Invesco's holdings JSON, and
  the iShares URLs now serve the `latest-holdings.csv` export, which has no
  CUSIP column. Parse a sponsor body with `parse_holdings` and the
  `DataSource` it came with rather than a fixed parser.
- `parse_ishares_csv` and `parse_invesco_csv` return an error when the
  header row is missing, where they returned an empty list.
- Every row stores its ticker in one spelling, `BRK.B`, whatever the source
  wrote. iShares rows carry no CUSIP and are keyed by ticker.
- `SponsorClient::fetch_today` accepts a body only if it parses as that
  sponsor's holdings file and lists most of the index.
- The default data origin, CDN mirror and repository link are under
  `kovagent/indexkit`. The SEC User-Agent default carries a placeholder
  contact; set `INDEXKIT_SEC_USER_AGENT` to a real one to run a backfill.
- Licensed MIT OR Apache-2.0, where 1.0 was Apache-2.0 only.

### Added

- **Russell 2000** (`IndexId::Rut`): quarterly holdings from IWM's N-PORT
  filings since 2019-12 and daily holdings from IWM's file.
- **Daily Dow Jones holdings**, from DIA's file, through the new SPDR XLSX
  parser `parse_spdr_xlsx`. (Closes #1.)
- **`latest(id)`**, `Indexkit::latest` and `latest_blocking`: the newest day
  of any index as a `DailySnapshot`, from the newest month's
  highest-priority source, never dated after today. A month that cannot be
  fetched and is not cached is passed over, so it works offline from the
  newest cached month; the snapshot's date says which day it is.
  (Closes #94.)
- **A backup source per index where one exists**, `sponsor_urls`: SPY then
  IVV for the S&P 500, IJH then MDY for the S&P 400, IJR then SPSM for the
  S&P 600, QQQ then QQQM for the Nasdaq-100; DIA for the Dow and IWM for the
  Russell 2000 have no second usable file. `retired_sponsor_urls` lists URLs
  read from Wayback captures only. (Closes #3.)
- `parse_invesco_dng_json` (Invesco's holdings JSON), `parse_nasdaq_ndx_json`
  (Nasdaq's list API, for its captures), and `parse_holdings`, which
  dispatches a body to its parser and errors on a wrong file or one with no
  members.
- `canonical_ticker` and `is_index_member`: the one spelling tickers are
  stored under, and the rule that tells an index member from the other
  lines a fund file carries.
- `indexkit-cli normalize` re-applies those rules to every stored month,
  rewrites only the months it changes and regenerates the manifest.

### Changed

- **Each day comes from one source.** Sources key rows differently: the
  N-PORT filing by CUSIP, the fund files and membership lists by ticker,
  and the hanshof list spells `BF-B` where fja spells `BF.B`. A day two of
  them covered listed each such member twice; nearly every stored S&P 500
  day listed `BF.B` twice, and each quarter-end day listed every member
  twice. `on`, `daily_range` and `latest` now take the day's
  highest-priority source; among equals, the one listing the most rows,
  then the index's primary.
- **The Nasdaq-100 comes from Invesco's QQQ holdings JSON, with QQQM as
  backup.** Invesco retired the QQQ CSV URL 1.0 read, which then answered
  with its homepage, so the Nasdaq-100 had no daily rows. Nasdaq's list API
  resets any client that identifies itself rather than presenting as a
  browser, so it is read from Wayback captures only. (Closes #6.)
- **The nightly fails when a fetch fails.** `daily-fetch` and
  `nightly-append` attempt every index, write what succeeded, and exit
  non-zero naming the indices that failed; the nightly then fails its run.
  Both used to exit 0, so the nightly stayed green for months while four
  indices wrote nothing.
- `wayback-backfill` reads every endpoint of an index, and the retired URLs
  whose captures hold the files served before each change, keeping one
  endpoint per capture date because its rows carry only that date. 1.0 read
  the primary URL only.
- The README coverage table states what the bundled data holds: quarterly
  history for the S&P 400, S&P 600, Nasdaq-100 and Russell 2000, and daily
  holdings with weights from 2026-04 (S&P 500, Dow) or 2026-09 (the
  others).
- CI checks that a data change is committed with its manifest; clients
  verify every file against the manifest and refuse a mismatch.
- Request User-Agents carry the crate version.

### Fixed

- **Members listed twice, and lines that are not members.** Every parser,
  the membership mirrors included, stores tickers through
  `canonical_ticker` (iShares' `BRK B` and SPDR's `BRK.B` are one ticker)
  and keeps only lines `is_index_member` accepts: not cash and
  money-market sweeps, index futures, rights, warrants or when-issued
  lines, contingent value rights, escrow, private, unlisted or contra lines.
  A line under a fund's internal placeholder code is kept when it carries a
  member's weight, because a fund lists a member it still holds that way
  while a corporate action is processed (SPY listed ExxonMobil as
  `2670549D` on 2026-07-01). An iShares member held only through a swap is
  kept at its notional weight. `normalize` applied the same rules to the
  bundled history: 15,658 S&P 500 tickers respelled, 108 rows that are not
  member lines dropped (residual, derivative, when-issued, right and
  warrant codes), 15,655 duplicate rows merged.
- **S&P 400 and S&P 600 daily holdings arrive again.** iShares dropped the
  quotes from its CSV header, which the parser did not recognise (#5), and
  later answered its old holdings URLs with the product page and status
  200, which was taken for the file and parsed to nothing. The header is
  recognised in both shapes, the iShares endpoints use the
  `latest-holdings.csv` export, and a body that is not holdings, or lists
  well under the index, falls through to the next endpoint. SSGA no longer
  serves SLY's file; SPSM is the S&P 600 backup. (Closes #5, #7, #95.)
- **`*_latest()` returned the whole month.** A caller who deduplicated the
  tickers kept members that had left mid-month. (Closes #96.)
- **`quick-xml` 0.41 and `calamine` 0.36** clear RUSTSEC-2026-0194 and
  RUSTSEC-2026-0195, which failed the nightly security sweep every day.
- The docs no longer promise a GICS sector "in v1.1", count five indices, or
  tie an index to one ETF.

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
