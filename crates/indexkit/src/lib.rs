//! `indexkit` -- index constituent service for Rust.
//!
//! Daily and monthly snapshots of the S&P 500, S&P MidCap 400, S&P SmallCap
//! 600, Nasdaq-100, and Dow Jones Industrial Average, served from bundled
//! parquet files with runtime fetch and local cache. No API keys. Offline
//! after the first successful fetch.
//!
//! Data is assembled from public regulatory filings and sponsor-published
//! holdings. Every row carries its origin in [`types::DataSource`].
//!
//! # Quick start
//!
//! ```no_run
//! use indexkit::{ym, IndexId};
//!
//! #[tokio::main]
//! async fn main() -> indexkit::Result<()> {
//!     // Free functions -- no client setup needed
//!     let sp500 = indexkit::sp500_latest().await?;
//!     let ndx   = indexkit::constituents_for(IndexId::Ndx, ym!(2024, 1)).await?;
//!
//!     println!("S&P 500 latest: {} holdings", sp500.len());
//!     println!("Top: {} at {:.2}%", sp500[0].name, sp500[0].weight * 100.0);
//!     println!("NDX Jan 2024: {} holdings", ndx.len());
//!     Ok(())
//! }
//! ```
//!
//! # Major types
//!
//! - [`Indexkit`] -- stateful client; create once, call many times.
//! - [`YearMonth`] -- year-month newtype; accepts strings, integers, tuples.
//! - [`Constituent`] -- one holding.
//! - [`IndexSnapshot`] -- constituents + metadata for one month.
//! - [`IndexId`] -- typed index identifier (Sp500, Sp400, Sp600, Ndx, Dji).
//! - [`Error`] -- unified error type; match on this, never on sub-types.
//!
//! # Environment overrides
//!
//! | Variable | Effect |
//! |---|---|
//! | `INDEXKIT_BASE_URL` | Replace the GitHub raw origin URL |
//! | `INDEXKIT_CACHE_DIR` | Override `~/.cache/indexkit/` |
//! | `INDEXKIT_MIRROR_URL` | CDN mirror fallback URL (default: jsDelivr) |
//!
//! # Field coverage
//!
//! Which columns a given [`Constituent`] carries depends on the row's
//! [`DataSource`]. Some rows are full-field (weight, shares, market value,
//! CUSIP); others are ticker-only, where `weight` is `f64::NAN`, `cusip` is
//! empty, and `shares` / `market_value_usd` are `0.0`. Use
//! [`Constituent::weight_opt`] for an `Option<f64>` accessor that returns
//! `None` on the NaN sentinel.
//!
//! # Limitations (v1.0.x)
//!
//! - **No GICS sector**: reserved for v1.1 via SIC -> GICS cross-walk.
//! - **Filing lag**: the regulatory-filing baseline trails real time;
//!   sponsor-published holdings close the recency gap.
//!
//! # Modules
//!
//! - [`client`] -- [`Indexkit`] async client with blocking wrappers.
//! - [`date`] -- [`YearMonth`] newtype for month inputs.
//! - [`types`] -- [`Constituent`], [`IndexSnapshot`], [`IndexId`],
//!   [`types::DataSource`].
//! - [`github_mirror`] -- historical constituent CSV fetchers.
//! - [`nport`] -- regulatory filing parser.
//! - [`sponsor`] -- sponsor holdings CSV parsers.
//! - [`wayback`] -- archived-snapshot client.
//! - [`cik`] -- ETF identifier mapping.
//! - [`parquet_io`] -- parquet writer + reader.
//! - [`sec`] -- filing client used by the CLI for backfill.
//! - [`coalesce`] -- merge rows from multiple sources into one snapshot.
//! - [`error`] -- unified [`Error`] enum and [`Result`] alias.

pub mod cik;
pub mod client;
pub mod coalesce;
pub mod date;
pub mod error;
pub(crate) mod fetcher;
pub mod github_mirror;
pub mod nport;
pub mod parquet_io;
pub mod sec;
pub mod sponsor;
pub mod types;
pub mod wayback;

// ---- Top-level re-exports ----

pub use client::Indexkit;
pub use date::{IntoYearMonth, YearMonth, YearMonthError};
pub use error::{Error, IndexkitError, Result};
pub use nport::{holdings_to_constituents, parse_nport, NportFiling, NportHeader, RawHolding};
pub use sec::{FilingRef, SecClient};
pub use sponsor::{parse_invesco_csv, parse_ishares_csv, SponsorClient};
pub use types::{
    Constituent, DailySnapshot, DataSource, IndexId, IndexSnapshot, Resolution, Sector,
};
pub use wayback::{WaybackClient, WaybackMatch};

// ---- Free-function shortcuts ----
//
// Each function internally uses a process-wide `Indexkit` instance so that
// multiple calls share one HTTP client and cache.

use std::sync::OnceLock;

fn global_client() -> &'static Indexkit {
    static CLIENT: OnceLock<Indexkit> = OnceLock::new();
    CLIENT.get_or_init(Indexkit::new)
}

/// Constituents for any index at any month (uses shared global client).
///
/// # Example
///
/// ```no_run
/// use indexkit::{IndexId, ym};
///
/// #[tokio::main]
/// async fn main() -> indexkit::Result<()> {
///     let cs = indexkit::constituents_for(IndexId::Sp500, ym!(2024, 1)).await?;
///     println!("{} holdings", cs.len());
///     Ok(())
/// }
/// ```
pub async fn constituents_for(id: IndexId, ym: impl IntoYearMonth) -> Result<Vec<Constituent>> {
    global_client().constituents_by_id(id, ym).await
}

/// Latest S&P 500 snapshot (uses shared global client).
///
/// # Example
///
/// ```no_run
/// #[tokio::main]
/// async fn main() -> indexkit::Result<()> {
///     let cs = indexkit::sp500_latest().await?;
///     println!("top holding: {}", cs[0].name);
///     Ok(())
/// }
/// ```
pub async fn sp500_latest() -> Result<Vec<Constituent>> {
    global_client().sp500_latest().await
}

/// Latest S&P 500 ticker list (uses shared global client).
///
/// Always returns an empty vector in v1.0 because N-PORT does not include
/// ticker symbols; retained for API compatibility with downstream consumers.
///
/// # Example
///
/// ```no_run
/// #[tokio::main]
/// async fn main() -> indexkit::Result<()> {
///     let _tickers = indexkit::sp500_tickers_latest().await?;
///     Ok(())
/// }
/// ```
pub async fn sp500_tickers_latest() -> Result<Vec<String>> {
    let cs = global_client().sp500_latest().await?;
    Ok(cs.into_iter().filter_map(|c| c.ticker).collect())
}

/// Latest Nasdaq-100 snapshot (uses shared global client).
pub async fn ndx_latest() -> Result<Vec<Constituent>> {
    global_client().ndx_latest().await
}

/// Latest DJIA snapshot (uses shared global client).
pub async fn dji_latest() -> Result<Vec<Constituent>> {
    global_client().dji_latest().await
}
