# indexkit

Daily index constituents for the S&P 500, S&P 400/600, Nasdaq-100, Dow Jones and Russell 2000, for Rust. Served from bundled parquet with on-demand fetch and a local cache. No API keys. Offline after the first query.

## Install

```toml
[dependencies]
indexkit = "2"
```

To track unreleased changes, depend on the repository directly:

```toml
indexkit = { git = "https://github.com/kovagent/indexkit" }
```

## Quick start

```rust,no_run
use indexkit::{ym, IndexId};

#[tokio::main]
async fn main() -> indexkit::Result<()> {
    // Free functions, no client setup needed.
    let sp500 = indexkit::sp500_latest().await?;
    let ndx = indexkit::constituents_for(IndexId::Ndx, ym!(2024, 1)).await?;
    let dji = indexkit::dji_latest().await?;
    let sp400 = indexkit::latest(IndexId::Sp400).await?; // any index, newest day

    println!("S&P 500 latest: {} holdings", sp500.len());
    println!("NDX Jan 2024: {} holdings", ndx.len());
    println!("DJIA latest: {} holdings", dji.len());
    println!("S&P 400 on {}: {} holdings", sp400.date, sp400.constituents.len());
    Ok(())
}
```

## Client pattern

```rust,no_run
use indexkit::{ym, Indexkit, YearMonth};

#[tokio::main]
async fn main() -> indexkit::Result<()> {
    let client = Indexkit::new(); // infallible, reuses one HTTP client and cache

    // Any month form works, no chrono import needed.
    let _ = client.sp500("2024-01").await?;
    let _ = client.sp500(202401u32).await?;
    let _ = client.sp500((2024i32, 1u32)).await?;
    let _ = client.sp500(ym!(2024, 1)).await?;
    let _ = client.sp500(YearMonth::new(2024, 1)?).await?;

    // Any index by id string.
    let _ndx = client.constituents("ndx", ym!(2024, 1)).await?;

    // Multi-month range.
    let history = client.sp500_range(ym!(2024, 1), ym!(2024, 12)).await?;
    println!("2024 S&P 500 snapshots: {}", history.len());

    // Weight lookup by CUSIP.
    if let Some(w) = client.weight("037833100", "sp500", ym!(2024, 1)).await? {
        println!("Apple weight: {:.2}%", w * 100.0);
    }

    // Blocking from sync code, no async runtime needed.
    let _ = client.sp500_blocking(ym!(2024, 1))?;
    Ok(())
}
```

## CLI

```bash
# Inspect one month
indexkit-cli get sp500 --month 2024-01

# Backfill local data for an index
indexkit-cli backfill --index ndx --start 2023-01

# Re-apply the ingestion rules (one ticker spelling, index members only) to stored months;
# regenerates data/manifest.json when it rewrites anything
indexkit-cli normalize

# Regenerate data/manifest.json after any other data change
indexkit-cli manifest

# Print what the bundled data covers per index (the table under Coverage)
indexkit-cli coverage
```

Run `indexkit-cli --help` for the full command list.

## Coverage

Every index runs from the month shown to the present.

<!-- coverage:start -->
Updated by the nightly from the bundled data:

| Index | History from | Daily holdings with weights from | Newest day |
|---|---|---|---|
| S&P 500 | 1996-01 | 2026-04 | 2026-09-24, 503 members |
| S&P MidCap 400 | 2019-12 | 2026-09 | 2026-09-23, 400 members |
| S&P SmallCap 600 | 2019-12 | 2026-09 | 2026-09-23, 603 members |
| Nasdaq-100 | 2019-12 | 2026-09 | 2026-09-24, 101 members |
| Dow Jones Industrial Average | 2020-01 | 2026-04 | 2026-09-24, 30 members |
| Russell 2000 | 2019-12 | 2026-09 | 2026-09-23, 1973 members |
<!-- coverage:end -->

Before daily holdings start, a day comes from the quarterly holdings (weights and CUSIPs, from 2019) or a membership list (tickers only: daily for the S&P 500 from 1996, monthly for the Nasdaq-100 and Dow from 2023-07). `latest(id)` returns the newest day, `on(id, date)` any other.

### Where each index comes from

| Index | Daily holdings | Quarterly holdings (N-PORT) | Membership lists |
|---|---|---|---|
| S&P 500 | SPY, with IVV as backup | IVV | fja05680 and hanshof (daily, to 2026-01), yfiua (monthly) |
| S&P MidCap 400 | IJH, with MDY as backup | IJH | - |
| S&P SmallCap 600 | IJR, with SPSM as backup | IJR | - |
| Nasdaq-100 | QQQ, with QQQM as backup | QQQ | yfiua (monthly) |
| Dow Jones Industrial Average | DIA | DIA | yfiua (monthly) |
| Russell 2000 | IWM | IWM | - |

The daily holdings are the funds' own published files, fetched each trading day, so daily coverage starts when fetching began, not with the fund. The quarterly holdings are the same funds' filings with the SEC, public from 2019. The membership lists come from the open datasets under [Attribution](#attribution); the two daily ones stopped updating upstream in 2026-01.

### What each kind of row carries

| Kind of source | Cadence | Ticker | CUSIP | Weight | Shares | Market value |
|---|---|---|---|---|---|---|
| Daily holdings, SPDR (SPY, MDY, SPSM, DIA) | Trading day | Yes | No | Yes | Yes | No |
| Daily holdings, iShares (IVV, IJH, IJR, IWM) | Trading day | Yes | No | Yes | Yes | Yes |
| Daily holdings, Invesco (QQQ, QQQM) | Trading day | Yes | Yes | Yes | Yes | No |
| Quarterly holdings (N-PORT) | Quarter-end | No | Yes | Yes | Yes | Yes |
| Membership lists | Daily or monthly | Yes | No | No | No | No |

A weight is a fraction of the fund's net assets (`0.0712` for 7.12%); where a source has none it is `NaN`, and `weight_opt()` returns `None`. Missing shares and market values are `0.0`. Tickers are stored in one spelling across sources, `BRK.B`.

## Data

Constituents are assembled from public regulatory filings, sponsor-published holdings, and permissively-licensed open datasets. Every row carries a `source` field recording its origin, so callers can filter by it. Parquet files live in `data/{index}/{index}-YYYY-MM.parquet`.

### Attribution

The bundled datasets include data from the open-source projects below, used under their respective licenses. Verbatim upstream LICENSE files ship in [`data/licenses/`](data/licenses/).

- [fja05680/sp500](https://github.com/fja05680/sp500) by Farrell J. Aultman, under MIT.
- [yfiua/index-constituents](https://github.com/yfiua/index-constituents), under Apache-2.0.
- [hanshof/sp500_constituents](https://github.com/hanshof/sp500_constituents) by running_error, under MIT.

Thanks to all three maintainers for keeping these datasets open.

## Cache

On first use, `Indexkit` downloads each month file and writes it to `~/.cache/indexkit/` (XDG-compliant via the `directories` crate). Subsequent calls check the SHA-256 digest listed in `data/manifest.json`; an unmodified cached file is returned immediately. On network failure the stale cached file is returned so existing workflows survive transient outages. A `ChecksumMismatch` error is returned if a downloaded file fails digest verification.

| Variable | Effect |
|---|---|
| `INDEXKIT_BASE_URL` | Replace the GitHub raw origin |
| `INDEXKIT_CACHE_DIR` | Override the cache directory |
| `INDEXKIT_MIRROR_URL` | CDN fallback URL (default: jsDelivr) |

## API

Full API reference is on [docs.rs](https://docs.rs/indexkit).

## License

Dual-licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).
