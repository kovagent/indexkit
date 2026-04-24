//! Unified error type for indexkit.
//!
//! All public methods return `indexkit::Result<T>` which is
//! `std::result::Result<T, indexkit::Error>`.

use thiserror::Error;

pub use crate::date::YearMonthError;

/// The single unified error type for indexkit operations.
///
/// Match on this enum when you need to distinguish error kinds; otherwise
/// `?` propagates it through any `Result<_, indexkit::Error>` context.
#[derive(Debug, Error)]
pub enum Error {
    /// A year-month value could not be parsed or validated.
    #[error("year-month error: {0}")]
    YearMonth(#[from] YearMonthError),

    /// Parquet file read or write failed.
    #[error("parquet I/O error: {0}")]
    Parquet(String),

    /// SEC EDGAR N-PORT fetch or parse failed.
    #[error("nport error: {0}")]
    Nport(String),

    /// XML parsing failed.
    #[error("xml error: {0}")]
    Xml(String),

    /// The requested index id is not in the CIK map (e.g. typo, unsupported).
    #[error("unknown index id: {0}")]
    UnknownIndex(String),

    /// Requested snapshot is not present in coverage
    /// (before Nov 2019, or a month that has no published filing yet).
    #[error("no data for {index} at {year_month}")]
    SnapshotNotFound { index: String, year_month: String },

    /// Underlying HTTP transport error.
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    /// I/O error (file system, tempfile, etc.).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Arrow columnar format error (from parquet reading).
    #[error("arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    /// Native parquet crate error.
    #[error("parquet error: {0}")]
    ParquetNative(#[from] parquet::errors::ParquetError),

    /// JSON parse error (cik-map, manifest).
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// SHA-256 digest of a fetched file does not match the manifest entry.
    ///
    /// The corrupt bytes were NOT written to the on-disk cache.
    #[error("checksum mismatch for {file}: expected sha256:{expected} got sha256:{actual}")]
    ChecksumMismatch {
        file: String,
        expected: String,
        actual: String,
    },

    /// Any other error not covered by the specific variants above.
    #[error("{0}")]
    Other(String),
}

/// `Result<T>` alias using [`enum@Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Alias for [`enum@Error`] kept for parity with sibling crates.
///
/// Code that references `indexkit::IndexkitError` compiles.
pub type IndexkitError = Error;

// anyhow bridge (one-way) for internal code still using anyhow.
impl From<anyhow::Error> for Error {
    fn from(e: anyhow::Error) -> Self {
        Error::Other(e.to_string())
    }
}
