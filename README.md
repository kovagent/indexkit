# indexkit

Index constituent service for Rust -- **daily** holdings of the S&P 500,
S&P MidCap 400, S&P SmallCap 600, Nasdaq-100, and Dow Jones Industrial
Average. Served from bundled parquet with runtime fetch and local cache.
No API keys. Offline after first query.

## Sources

Constituents are assembled from public regulatory filings, sponsor-published
holdings, and permissively-licensed open datasets (credited under
[Attribution](#attribution)). Every row is stamped with a `source` column
(see [`DataSource`](https://docs.rs/indexkit/latest/indexkit/types/enum.DataSource.html))
so callers can filter by confidence. When rows from multiple sources cover
the same `(identity, date)` key, the coalescer keeps a single best row;
`identity` is CUSIP where present, falling back to ticker for ticker-only
sources.

[fja05680/sp500]: https://github.com/fja05680/sp500
[yfiua/index-constituents]: https://github.com/yfiua/index-constituents
[hanshof/sp500_constituents]: https://github.com/hanshof/sp500_constituents

### Field coverage by source

Ticker-only sources (the three GitHub mirrors) produce rows where
`cusip`, `lei`, `issuer_cik` are empty, `shares` and `market_value_usd`
are `0.0`, and `weight` is `f64::NAN`. Use [`Constituent::weight_opt()`](https://docs.rs/indexkit/latest/indexkit/types/struct.Constituent.html#method.weight_opt)
for an `Option<f64>` that returns `None` on the NaN sentinel, or
[`IndexSnapshot::has_weights()`](https://docs.rs/indexkit/latest/indexkit/types/struct.IndexSnapshot.html#method.has_weights)
for a quick "is this a weight vector or just a ticker universe" gate.

## Install

```toml
# Cargo.toml
[dependencies]
indexkit = "1.0"
```

Or from source:

```toml
indexkit = { git = "https://github.com/userFRM/indexkit" }
```

## Quick start -- one-off scripts

```rust
use indexkit::{ym, IndexId};

#[tokio::main]
async fn main() -> indexkit::Result<()> {
    // Free functions -- no client setup, no chrono import
    let sp500  = indexkit::sp500_latest().await?;
    let ndx    = indexkit::constituents_for(IndexId::Ndx, ym!(2024, 1)).await?;
    let dji    = indexkit::dji_latest().await?;

    println!("S&P 500 latest: {} holdings", sp500.len());
    println!("Top: {} at {:.2}%", sp500[0].name, sp500[0].weight * 100.0);
    println!("NDX Jan 2024: {} holdings", ndx.len());
    println!("DJIA latest: {} holdings", dji.len());
    Ok(())
}
```

## Client pattern -- connection pool + cache reuse

```rust
use indexkit::{Indexkit, ym, YearMonth};

#[tokio::main]
async fn main() -> indexkit::Result<()> {
    let client = Indexkit::new();   // infallible, no ?

    // Any month form works -- no chrono import needed
    let cs = client.sp500("2024-01").await?;
    let cs = client.sp500(202401u32).await?;
    let cs = client.sp500((2024i32, 1u32)).await?;
    let cs = client.sp500(ym!(2024, 1)).await?;
    let cs = client.sp500(YearMonth::new(2024, 1)?).await?;

    // Any index by id string
    let ndx = client.constituents("ndx", ym!(2024, 1)).await?;

    // Multi-month range
    let history = client.sp500_range(ym!(2024, 1), ym!(2024, 12)).await?;
    println!("2024 S&P 500 snapshots: {}", history.len());

    // Weight lookup by CUSIP
    if let Some(w) = client.weight("037833100", "sp500", ym!(2024, 1)).await? {
        println!("Apple weight: {:.2}%", w * 100.0);
    }

    // Blocking from sync code -- no async runtime needed
    let _ = client.sp500_blocking(ym!(2024, 1))?;

    let _ = cs;
    Ok(())
}
```

## Daily resolution

```rust
use indexkit::Indexkit;
use chrono::NaiveDate;

#[tokio::main]
async fn main() -> indexkit::Result<()> {
    let client = Indexkit::new();

    // S&P 500 on a specific business day -- returns rows from the best
    // source available on or before that date.
    let cs = client
        .sp500_on(NaiveDate::from_ymd_opt(2024, 3, 20).unwrap())
        .await?;

    // Daily range -- one DailySnapshot per distinct date with data.
    let days = client
        .sp500_daily_range(
            NaiveDate::from_ymd_opt(2024, 3, 1).unwrap(),
            NaiveDate::from_ymd_opt(2024, 3, 31).unwrap(),
        )
        .await?;
    println!("{} daily snapshots in March 2024", days.len());

    // Resolution probe -- classify a month as Daily / Sparse / Monthly / None.
    let r = client.resolution("sp500", "2024-03").await?;
    println!("March 2024 resolution: {r:?}");
    Ok(())
}
```

## CLI

```bash
# SEC N-PORT monthly baseline backfill (guaranteed to work, public domain)
indexkit-cli backfill
indexkit-cli backfill --index ndx --start 2023-01

# Forward-going sponsor-CDN daily fetch (each sponsor's ToS apply)
indexkit-cli daily-fetch --accept-sponsor-tos
indexkit-cli daily-fetch --index sp500 --accept-sponsor-tos

# Historical daily backfill from Wayback Machine
indexkit-cli wayback-backfill --index sp500
indexkit-cli wayback-backfill --index sp500 --from 2021-01-01 --to 2021-12-31

# OSS GitHub mirrors (permissive licenses, massive historical coverage)
indexkit-cli github-backfill                   # all three (fja05680 + yfiua + hanshof)
indexkit-cli github-backfill --source fja05680 # S&P 500 daily 1996+ (MIT)
indexkit-cli github-backfill --source yfiua    # sp500 / ndx / dji monthly, 2018+ (Apache-2.0)
indexkit-cli github-backfill --source hanshof  # S&P 500 daily 1996+ cross-check (MIT)

# Append newly-published N-PORT filings (used by nightly CI)
indexkit-cli nightly-append

# Inspect a month
indexkit-cli get sp500 --month 2024-01
indexkit-cli get ndx --month 2024-01

# Regenerate data/manifest.json after a data change
indexkit-cli manifest

# Write data/cik-map.json
indexkit-cli cik-map
```

## API surface

### Free functions (one-off scripts)

| Function | Returns |
|---|---|
| `sp500_latest()` | `Result<Vec<Constituent>>` |
| `ndx_latest()` | `Result<Vec<Constituent>>` |
| `dji_latest()` | `Result<Vec<Constituent>>` |
| `constituents_for(id, ym)` | `Result<Vec<Constituent>>` |
| `sp500_tickers_latest()` | `Result<Vec<String>>` (empty in v1.0 -- N-PORT has no ticker) |

### Client methods -- per index

| Method | Returns |
|---|---|
| `sp500(ym)` | `Result<Vec<Constituent>>` |
| `sp500_latest()` | `Result<Vec<Constituent>>` |
| `sp500_range(start, end)` | `Result<Vec<IndexSnapshot>>` |
| `sp400(ym)` / `sp400_latest()` / `sp400_range(...)` | same shape |
| `sp600(ym)` / `sp600_latest()` / `sp600_range(...)` | same shape |
| `ndx(ym)`   / `ndx_latest()`   / `ndx_range(...)`   | same shape |
| `dji(ym)`   / `dji_latest()`   / `dji_range(...)`   | same shape |

### Client methods -- generic

| Method | Returns |
|---|---|
| `constituents(index, ym)` | `Result<Vec<Constituent>>` |
| `constituents_by_id(id, ym)` | `Result<Vec<Constituent>>` |
| `snapshot(id, ym)` | `Result<IndexSnapshot>` |
| `tickers(index, ym)` | `Result<Vec<String>>` |
| `weight(cusip_or_name, index, ym)` | `Result<Option<f64>>` |

### Client methods -- blocking (sync)

| Method | Returns |
|---|---|
| `sp500_blocking(ym)` / `sp500_latest_blocking()` | `Result<Vec<Constituent>>` |
| `ndx_blocking(ym)`   / `ndx_latest_blocking()`   | `Result<Vec<Constituent>>` |
| `dji_blocking(ym)`   / `dji_latest_blocking()`   | `Result<Vec<Constituent>>` |
| `constituents_blocking(index, ym)` | `Result<Vec<Constituent>>` |

### Month inputs

Every method that takes a month accepts any of these forms via `impl IntoYearMonth`:

```rust
client.sp500("2024-01").await?          // ISO string
client.sp500("2024/01").await?          // slashed string
client.sp500("202401").await?           // compact string
client.sp500(202401u32).await?          // YYYYMM integer
client.sp500((2024i32, 1u32)).await?    // (year, month) tuple
client.sp500(ym!(2024, 1)).await?       // ym! macro
client.sp500(YearMonth::new(2024, 1)?).await?  // explicit
```

## Constituent struct

```rust
pub struct Constituent {
    pub ticker: Option<String>,      // populated for CDN/Wayback rows; None for N-PORT
    pub name: String,
    pub cusip: String,               // 9-char -- primary join key
    pub lei: Option<String>,
    pub shares: f64,                 // can be fractional
    pub market_value_usd: f64,
    pub weight: f64,                 // fraction of NAV in 0..1
    pub issuer_cik: Option<String>,  // reserved
    pub sector: Option<Sector>,      // reserved for v1.1
    pub as_of: NaiveDate,            // business day priced
    pub source: DataSource,          // IsharesCdn / InvescoCdn / SpdrCdn / Wayback(date) / SecNport
}
```

## Data

| Source | Coverage | Latency |
|---|---|---|
| Sponsor CDN (iShares IVV/IJH/IJR, Invesco QQQ, SPDR DIA) | forward-going | T+1 daily |
| Wayback Machine (archive.org) | 2019-11 - present, ~40-60 % days | per-snapshot |
| SEC N-PORT (IVV, IJH, IJR, QQQ, DIA) | 2019-11 - present | T+90d monthly |

Parquet files live in `data/{index}/{index}-YYYY-MM.parquet`. Each month
file contains rows from every available source; the coalescer keeps only a
single best row per `(cusip, as_of)`.

Data is updated by three GitHub Actions workflows:

- **nightly.yml** -- cron `0 7 * * 1-5` (07:00 UTC, Mon-Fri): pulls any
  newly-published N-PORT filings and, if enabled, fetches today's sponsor
  CDN holdings.
- **backfill.yml** -- `workflow_dispatch`: full historical fetch (all
  five indices, N-PORT baseline back to 2019-11).
- **release.yml** -- on `v*` tag push: CI gate + `cargo publish`.

## Limitations (v1.0)

1. **Ticker coverage varies by source**: Sponsor-CDN rows include ticker
   for ~99 % of holdings. Wayback rows inherit from the underlying CDN
   capture. **N-PORT rows have no ticker** -- use CUSIP as the join key,
   which is present everywhere.

2. **No GICS sector**: `Constituent::sector` is always `None` in v1.0.
   Planned for v1.1 via SIC -> GICS cross-walk.

3. **N-PORT baseline is 60-90 days behind real time**: ETFs file N-PORT
   ~60 days after each reporting period end, and the SEC delays public
   release by another 30 days. Sponsor CDN closes this gap (T+1) where
   the live fetcher is permitted by ToS.

4. **Wayback coverage is patchy**: Internet Archive captures roughly 40-
   60 % of trading days for each sponsor URL, depending on how often the
   page was crawled. Expect gaps; they fall back to N-PORT monthly.

5. **SPDR DIA daily = Wayback-only for v1.0**: State Street publishes DIA
   holdings as `.xlsx`. indexkit v1.0 does not parse XLSX; Wayback
   snapshots are the primary daily source for DJIA in v1.0. A future
   release may add XLSX handling.

6. **Coverage starts 2019-11**: SEC public N-PORT filing began Q4 2019.
   Wayback snapshots also cluster around that era and later because that
   is when retail interest in ETF holdings spiked.

7. **Sponsor-CDN ToS**: the live daily fetch is gated behind
   `--accept-sponsor-tos`. Review each sponsor's Terms of Service before
   running it -- indexkit makes no representations about permissiveness.

## v1.0 -- stability guarantees

- **Semver**: public API follows [Semantic Versioning](https://semver.org/).
  Breaking changes require a major version bump. Deprecations are announced
  one minor version before removal.
- **MSRV**: Rust stable >= 1.75 (edition 2021). MSRV changes are a minor bump.
- **Re-export policy**: every type and function re-exported from `indexkit::*`
  is considered public API. Internal modules (`fetcher`) are `pub(crate)`
  and not covered.
- **Error stability**: `indexkit::Error` variants are stable; adding new
  variants is a minor bump, removing them is a major bump.

## Cache

On first use, `Indexkit` downloads each month file from
`raw.githubusercontent.com/userFRM/indexkit/main/data/` and writes it to
`~/.cache/indexkit/` (XDG-compliant via the `directories` crate). Subsequent
calls check the SHA-256 digest listed in `data/manifest.json`; an unmodified
cached file is returned immediately. On network failure the stale cached
file is returned so existing workflows survive transient outages.

Each file's SHA-256 digest is verified against `manifest.json` before being
written to cache. A `ChecksumMismatch` error is returned if verification fails.

**Env overrides:**

| Variable | Effect |
|---|---|
| `INDEXKIT_BASE_URL` | Replace the GitHub raw origin |
| `INDEXKIT_CACHE_DIR` | Override the cache directory |
| `INDEXKIT_MIRROR_URL` | CDN fallback URL (default: jsDelivr) |

See [`docs/architecture.md`](docs/architecture.md) for the full data-flow
diagram and [`docs/data-sources.md`](docs/data-sources.md) for upstream URL
details.

## Crates

| Crate | Description |
|---|---|
| `indexkit` | Library -- fetcher, cache, types, N-PORT parser |
| `indexkit-cli` | Binary -- backfill, nightly-append, get |

## Attribution

indexkit v1.0.1 ingests historical constituent data from three OSS
GitHub repositories. Verbatim upstream LICENSE files ship in
[`data/licenses/`](data/licenses/); each source is credited in the
`source` column of the parquet output.

- **[fja05680/sp500]** by Farrell J. Aultman (MIT) -- S&P 500 daily
  historical components and changes, 1996-01-02 to present. indexkit's
  `DataSource::GithubFja05680` rows are derived from this dataset.
- **[yfiua/index-constituents]** (Apache-2.0) -- monthly snapshots of
  the S&P 500, Nasdaq-100, Dow Jones and several non-US indices, with
  tooling to scrape them from Wikipedia and equivalent sources.
  indexkit's `DataSource::GithubYfiua` rows are derived from this
  dataset.
- **[hanshof/sp500_constituents]** by running_error (MIT) -- S&P 500
  daily historical components, 1996 to present. indexkit's
  `DataSource::GithubHanshof` rows are derived from this dataset and
  used as a cross-check layer for fja05680.

Thanks to all three upstream maintainers for keeping these datasets
open.

## License

Apache-2.0 -- see [`LICENSE`](LICENSE).

The derivative parquet files in `data/` combine public-domain SEC EDGAR
filings, Internet Archive snapshots, and the three OSS GitHub mirrors
listed above. Each source is identified per-row via the `source`
column; upstream license terms (MIT and Apache-2.0) are retained for
the OSS sources and are shipped verbatim in `data/licenses/`.

Copyright 2026 userFRM
