# indexkit

Daily index constituents for the S&P 500, S&P 400/600, Nasdaq-100 and Dow Jones, for Rust. Served from bundled parquet with on-demand fetch and a local cache. No API keys. Offline after the first query.

## Install

```toml
[dependencies]
indexkit = "1.0"
```

To track unreleased changes, depend on the repository directly:

```toml
indexkit = { git = "https://github.com/userFRM/indexkit" }
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

    println!("S&P 500 latest: {} holdings", sp500.len());
    println!("NDX Jan 2024: {} holdings", ndx.len());
    println!("DJIA latest: {} holdings", dji.len());
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

# Regenerate data/manifest.json after a data change
indexkit-cli manifest
```

Run `indexkit-cli --help` for the full command list.

## Coverage

| Index | Granularity | History |
|---|---|---|
| S&P 500 | Daily | 1996-01 to present |
| S&P MidCap 400 | Daily | 2019-11 to present |
| S&P SmallCap 600 | Daily | 2019-11 to present |
| Nasdaq-100 | Daily | 2019-11 to present |
| Dow Jones Industrial Average | Daily | 2019-11 to present |

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
