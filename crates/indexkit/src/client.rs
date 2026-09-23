//! Stateful `Indexkit` client -- flat async endpoint methods.
//!
//! Fetches parquet files from GitHub raw (or a configurable origin) with an
//! XDG-compliant local cache + ETag revalidation. Falls back to stale cache
//! on network errors so existing workflows survive transient outages.
//!
//! # Example
//!
//! ```no_run
//! use indexkit::{Indexkit, ym};
//!
//! #[tokio::main]
//! async fn main() -> indexkit::Result<()> {
//!     let client = Indexkit::new();   // infallible
//!
//!     // S&P 500 constituents for a given month
//!     let cs = client.sp500(ym!(2024, 1)).await?;
//!     println!("top: {} at {:.2}%", cs[0].name, cs[0].weight * 100.0);
//!
//!     // Latest available
//!     let latest = client.sp500_latest().await?;
//!     println!("latest snapshot: {} holdings", latest.len());
//!
//!     Ok(())
//! }
//! ```

use chrono::{Datelike, NaiveDate};
use futures::future::try_join_all;
use std::path::PathBuf;

use crate::date::{IntoYearMonth, YearMonth};
use crate::error::{Error, Result};
use crate::fetcher::{default_cache_dir, resolved_base_url, CachedFetcher};
use crate::parquet_io::read_month;
use crate::types::{Constituent, DailySnapshot, IndexId, IndexSnapshot, Resolution};

/// Stateful indexkit client.
///
/// Wraps an ETag-aware cached fetcher and exposes flat endpoint methods.
/// Create once and reuse across calls; the internal reqwest client is kept
/// alive for connection pooling.
///
/// # Infallible construction
///
/// ```no_run
/// use indexkit::Indexkit;
/// let client = Indexkit::new();   // never fails
/// ```
///
/// # Builder pattern
///
/// ```no_run
/// use indexkit::Indexkit;
/// use std::path::PathBuf;
///
/// let client = Indexkit::new()
///     .with_base_url("https://my-mirror.example.com/indexkit")
///     .with_cache_dir(PathBuf::from("/tmp/indexkit-test"));
/// ```
pub struct Indexkit {
    fetcher: CachedFetcher,
}

impl Indexkit {
    /// Create a client with the default GitHub raw backend and XDG cache.
    ///
    /// Reads `INDEXKIT_BASE_URL` and `INDEXKIT_CACHE_DIR` from the environment
    /// if set, otherwise uses the GitHub raw origin and `~/.cache/indexkit/`.
    ///
    /// **This function never fails.** If the underlying HTTP client cannot be
    /// built (essentially only on exotic platforms with broken TLS), the error
    /// is deferred to the first fetch call. Use [`try_new`][Self::try_new] for
    /// early detection.
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent("indexkit/1.0 (+https://github.com/userFRM/indexkit)")
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            fetcher: CachedFetcher::new(http, resolved_base_url(), default_cache_dir()),
        }
    }

    /// Create a client with early failure detection.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the underlying reqwest client cannot be constructed.
    pub fn try_new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent("indexkit/1.0 (+https://github.com/userFRM/indexkit)")
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Self {
            fetcher: CachedFetcher::new(http, resolved_base_url(), default_cache_dir()),
        })
    }

    /// Override the origin URL.
    ///
    /// Default: `https://raw.githubusercontent.com/userFRM/indexkit/main/data`.
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.fetcher.set_base_url(url.into());
        self
    }

    /// Override the on-disk cache directory.
    ///
    /// Default: `~/.cache/indexkit/`.
    pub fn with_cache_dir(mut self, dir: PathBuf) -> Self {
        self.fetcher.set_cache_dir(dir);
        self
    }

    /// Override the CDN mirror URL used when the primary fetch fails.
    ///
    /// - `Some(url)` -- use a custom mirror.
    /// - `None` -- disable mirror fallback entirely.
    ///
    /// Equivalent to `INDEXKIT_MIRROR_URL` env var. Builder form wins.
    pub fn with_mirror_url(mut self, url: Option<String>) -> Self {
        self.fetcher.set_mirror_url(url);
        self
    }

    // ---- Generic constituents endpoint ----

    /// Fetch constituents for any supported index at a given month.
    ///
    /// `index` is a short id: `"sp500"`, `"sp400"`, `"sp600"`, `"ndx"`, `"dji"`.
    ///
    /// # Errors
    ///
    /// - [`Error::UnknownIndex`] if `index` is not recognised.
    /// - [`Error::SnapshotNotFound`] if the month has no published snapshot.
    /// - Network errors with no cached file.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use indexkit::{Indexkit, ym};
    /// # async fn run() -> indexkit::Result<()> {
    /// let client = Indexkit::new();
    /// let cs = client.constituents("ndx", ym!(2024, 1)).await?;
    /// # Ok(()) }
    /// ```
    pub async fn constituents(
        &self,
        index: &str,
        ym: impl IntoYearMonth,
    ) -> Result<Vec<Constituent>> {
        let ym = ym.into_year_month()?;
        let id =
            IndexId::from_str_id(index).ok_or_else(|| Error::UnknownIndex(index.to_string()))?;
        self.load_month(id, ym).await
    }

    /// Fetch constituents by typed [`IndexId`].
    pub async fn constituents_by_id(
        &self,
        id: IndexId,
        ym: impl IntoYearMonth,
    ) -> Result<Vec<Constituent>> {
        let ym = ym.into_year_month()?;
        self.load_month(id, ym).await
    }

    /// Fetch an [`IndexSnapshot`] -- the month and constituents bundled.
    pub async fn snapshot(&self, id: IndexId, ym: impl IntoYearMonth) -> Result<IndexSnapshot> {
        let ym = ym.into_year_month()?;
        let constituents = self.load_month(id, ym).await?;
        Ok(IndexSnapshot {
            index: id,
            year_month: ym,
            constituents,
        })
    }

    // ---- Sugar methods per index ----

    /// S&P 500 constituents (via IVV) for the given month.
    pub async fn sp500(&self, ym: impl IntoYearMonth) -> Result<Vec<Constituent>> {
        self.constituents_by_id(IndexId::Sp500, ym).await
    }

    /// S&P 500 latest available snapshot: the newest day only (see [`latest`][Self::latest]).
    pub async fn sp500_latest(&self) -> Result<Vec<Constituent>> {
        Ok(self.latest(IndexId::Sp500).await?.constituents)
    }

    /// S&P 500 snapshots for every month in `[start, end]` (inclusive).
    pub async fn sp500_range(
        &self,
        start: impl IntoYearMonth,
        end: impl IntoYearMonth,
    ) -> Result<Vec<IndexSnapshot>> {
        self.range(IndexId::Sp500, start, end).await
    }

    /// S&P 400 Mid-Cap constituents (via IJH) for the given month.
    pub async fn sp400(&self, ym: impl IntoYearMonth) -> Result<Vec<Constituent>> {
        self.constituents_by_id(IndexId::Sp400, ym).await
    }

    /// S&P 400 latest available: the newest day only (see [`latest`][Self::latest]).
    pub async fn sp400_latest(&self) -> Result<Vec<Constituent>> {
        Ok(self.latest(IndexId::Sp400).await?.constituents)
    }

    /// S&P 400 snapshots for every month in `[start, end]`.
    pub async fn sp400_range(
        &self,
        start: impl IntoYearMonth,
        end: impl IntoYearMonth,
    ) -> Result<Vec<IndexSnapshot>> {
        self.range(IndexId::Sp400, start, end).await
    }

    /// S&P 600 Small-Cap constituents (via IJR).
    pub async fn sp600(&self, ym: impl IntoYearMonth) -> Result<Vec<Constituent>> {
        self.constituents_by_id(IndexId::Sp600, ym).await
    }

    /// S&P 600 latest available: the newest day only (see [`latest`][Self::latest]).
    pub async fn sp600_latest(&self) -> Result<Vec<Constituent>> {
        Ok(self.latest(IndexId::Sp600).await?.constituents)
    }

    /// S&P 600 snapshots for every month in `[start, end]`.
    pub async fn sp600_range(
        &self,
        start: impl IntoYearMonth,
        end: impl IntoYearMonth,
    ) -> Result<Vec<IndexSnapshot>> {
        self.range(IndexId::Sp600, start, end).await
    }

    /// Nasdaq-100 constituents (via QQQ).
    pub async fn ndx(&self, ym: impl IntoYearMonth) -> Result<Vec<Constituent>> {
        self.constituents_by_id(IndexId::Ndx, ym).await
    }

    /// Nasdaq-100 latest available: the newest day only (see [`latest`][Self::latest]).
    pub async fn ndx_latest(&self) -> Result<Vec<Constituent>> {
        Ok(self.latest(IndexId::Ndx).await?.constituents)
    }

    /// Nasdaq-100 snapshots for every month in `[start, end]`.
    pub async fn ndx_range(
        &self,
        start: impl IntoYearMonth,
        end: impl IntoYearMonth,
    ) -> Result<Vec<IndexSnapshot>> {
        self.range(IndexId::Ndx, start, end).await
    }

    /// Dow Jones Industrial Average constituents (via DIA).
    pub async fn dji(&self, ym: impl IntoYearMonth) -> Result<Vec<Constituent>> {
        self.constituents_by_id(IndexId::Dji, ym).await
    }

    /// DJIA latest available: the newest day only (see [`latest`][Self::latest]).
    pub async fn dji_latest(&self) -> Result<Vec<Constituent>> {
        Ok(self.latest(IndexId::Dji).await?.constituents)
    }

    /// DJIA snapshots for every month in `[start, end]`.
    pub async fn dji_range(
        &self,
        start: impl IntoYearMonth,
        end: impl IntoYearMonth,
    ) -> Result<Vec<IndexSnapshot>> {
        self.range(IndexId::Dji, start, end).await
    }

    /// The newest snapshot of any index: every row dated the most recent day
    /// in the most recent month that has data, searching back from the
    /// current month up to six months.
    ///
    /// A month file holds every day fetched that month, so returning the
    /// month would also return members that left before its last day.
    ///
    /// # Errors
    ///
    /// - [`Error::SnapshotNotFound`] if none of those months has data.
    /// - Network errors with no cached file, rather than an older month.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use indexkit::{Indexkit, IndexId};
    /// # async fn run() -> indexkit::Result<()> {
    /// let snap = Indexkit::new().latest(IndexId::Sp400).await?;
    /// let tickers: Vec<&str> = snap.constituents.iter().filter_map(|c| c.ticker.as_deref()).collect();
    /// println!("S&P 400 on {}: {} names", snap.date, tickers.len());
    /// # Ok(()) }
    /// ```
    pub async fn latest(&self, id: IndexId) -> Result<DailySnapshot> {
        let mut ym = YearMonth::current_utc();
        for _ in 0..7 {
            match self.load_month(id, ym).await {
                Ok(rows) => {
                    if let Some(date) = rows.iter().map(|r| r.as_of).max() {
                        let newest = rows.into_iter().filter(|r| r.as_of == date).collect();
                        return Ok(day_snapshot(id, date, newest));
                    }
                }
                Err(Error::SnapshotNotFound { .. }) => {}
                Err(e) => return Err(e),
            }
            ym = ym.prev();
        }
        Err(Error::SnapshotNotFound {
            index: id.to_string(),
            year_month: "latest".to_string(),
        })
    }

    // ---- helpers ----

    /// Tickers for an index at a month, from the rows that carry one.
    ///
    /// Rows from sponsor holdings files carry a ticker; N-PORT rows do not,
    /// so a month served only from N-PORT yields none. Use CUSIP as the join
    /// key for those.
    pub async fn tickers(&self, index: &str, ym: impl IntoYearMonth) -> Result<Vec<String>> {
        let cs = self.constituents(index, ym).await?;
        Ok(cs.into_iter().filter_map(|c| c.ticker).collect())
    }

    /// Weight of a holding matched by CUSIP (preferred) or name fallback.
    ///
    /// `id` can be a CUSIP (9 chars, digits+letters) or a name substring.
    pub async fn weight(
        &self,
        id: &str,
        index: &str,
        ym: impl IntoYearMonth,
    ) -> Result<Option<f64>> {
        let cs = self.constituents(index, ym).await?;
        let by_cusip = cs.iter().find(|c| c.cusip == id);
        if let Some(c) = by_cusip {
            return Ok(Some(c.weight));
        }
        let by_name = cs.iter().find(|c| c.name.contains(id));
        Ok(by_name.map(|c| c.weight))
    }

    // ---- Daily resolution endpoints ----

    /// Return all rows for `index` on `date` -- daily granularity.
    ///
    /// Behaviour: load the parent month parquet, coalesce by priority
    /// (CDN > Wayback > N-PORT), and filter to rows whose `as_of == date`.
    /// If no row matches exactly, returns rows from the nearest previous
    /// business day within the same month (or the monthly baseline if
    /// that is all that exists).
    pub async fn on(&self, index: &str, date: NaiveDate) -> Result<Vec<Constituent>> {
        let id =
            IndexId::from_str_id(index).ok_or_else(|| Error::UnknownIndex(index.to_string()))?;
        self.on_by_id(id, date).await
    }

    /// As [`on`][Self::on] but typed [`IndexId`].
    pub async fn on_by_id(&self, id: IndexId, date: NaiveDate) -> Result<Vec<Constituent>> {
        let ym = YearMonth::new(date.year(), date.month())?;
        let month_rows = self.load_month(id, ym).await?;

        // Exact match.
        let exact: Vec<Constituent> = month_rows
            .iter()
            .filter(|c| c.as_of == date)
            .cloned()
            .collect();
        if !exact.is_empty() {
            return Ok(exact);
        }
        // Nearest previous day within the month.
        let mut by_day: Vec<chrono::NaiveDate> = month_rows.iter().map(|r| r.as_of).collect();
        by_day.sort_unstable();
        by_day.dedup();
        let chosen = by_day.iter().rev().find(|&&d| d <= date).copied();
        if let Some(d) = chosen {
            return Ok(month_rows.into_iter().filter(|c| c.as_of == d).collect());
        }
        // No daily data: return the first date in the month (N-PORT baseline).
        if let Some(first) = by_day.first().copied() {
            return Ok(month_rows
                .into_iter()
                .filter(|c| c.as_of == first)
                .collect());
        }
        Err(Error::SnapshotNotFound {
            index: id.to_string(),
            year_month: ym.to_string(),
        })
    }

    /// Return daily snapshots for `index` in `[start, end]` (inclusive), one
    /// per distinct `as_of` date present in the data.
    pub async fn daily_range(
        &self,
        id: IndexId,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<DailySnapshot>> {
        if start > end {
            return Err(Error::Other(format!(
                "daily_range: start {start} > end {end}"
            )));
        }
        let start_ym = YearMonth::new(start.year(), start.month())?;
        let end_ym = YearMonth::new(end.year(), end.month())?;
        let months: Vec<YearMonth> = start_ym.iter_to(end_ym).collect();

        let fetches = months.iter().map(|&ym| async move {
            match self.load_month(id, ym).await {
                Ok(rows) => Ok::<Vec<Constituent>, Error>(rows),
                Err(_) => Ok(Vec::new()),
            }
        });
        let all = try_join_all(fetches).await?;
        let mut flat: Vec<Constituent> = all.into_iter().flatten().collect();
        flat.retain(|r| r.as_of >= start && r.as_of <= end);

        // Group by (as_of).
        let mut by_day: std::collections::BTreeMap<NaiveDate, Vec<Constituent>> =
            Default::default();
        for r in flat {
            by_day.entry(r.as_of).or_default().push(r);
        }
        Ok(by_day
            .into_iter()
            .map(|(date, rows)| day_snapshot(id, date, rows))
            .collect())
    }

    // -- index-specific daily sugar --

    /// S&P 500 constituents on a specific business day.
    pub async fn sp500_on(&self, date: NaiveDate) -> Result<Vec<Constituent>> {
        self.on_by_id(IndexId::Sp500, date).await
    }

    /// Nasdaq-100 constituents on a specific business day.
    pub async fn ndx_on(&self, date: NaiveDate) -> Result<Vec<Constituent>> {
        self.on_by_id(IndexId::Ndx, date).await
    }

    /// DJIA constituents on a specific business day.
    pub async fn dji_on(&self, date: NaiveDate) -> Result<Vec<Constituent>> {
        self.on_by_id(IndexId::Dji, date).await
    }

    /// S&P 500 daily snapshots in `[start, end]`.
    pub async fn sp500_daily_range(
        &self,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<DailySnapshot>> {
        self.daily_range(IndexId::Sp500, start, end).await
    }

    /// Resolution hint for `index` at month `ym`.
    ///
    /// Inspects the stored month parquet and classifies:
    /// - `Daily` -- at least one row from a CDN or Wayback source and
    ///   coverage spans 15+ distinct dates (roughly 3 weeks of trading days).
    /// - `Sparse` -- daily-source rows exist but cover fewer dates.
    /// - `Monthly` -- only N-PORT rows present.
    /// - `None` -- the month is missing entirely.
    pub async fn resolution(&self, index: &str, ym: impl IntoYearMonth) -> Result<Resolution> {
        let ym = ym.into_year_month()?;
        let id =
            IndexId::from_str_id(index).ok_or_else(|| Error::UnknownIndex(index.to_string()))?;
        let rows = match self.load_month(id, ym).await {
            Ok(r) => r,
            Err(Error::SnapshotNotFound { .. }) => return Ok(Resolution::None),
            Err(e) => return Err(e),
        };
        if rows.is_empty() {
            return Ok(Resolution::None);
        }
        let mut distinct_days: std::collections::BTreeSet<chrono::NaiveDate> = Default::default();
        let mut has_daily_source = false;
        for r in &rows {
            distinct_days.insert(r.as_of);
            match r.source {
                crate::types::DataSource::IsharesCdn
                | crate::types::DataSource::InvescoCdn
                | crate::types::DataSource::SpdrCdn
                | crate::types::DataSource::Wayback(_) => has_daily_source = true,
                _ => {}
            }
        }
        if has_daily_source && distinct_days.len() >= 15 {
            Ok(Resolution::Daily)
        } else if has_daily_source {
            Ok(Resolution::Sparse)
        } else {
            Ok(Resolution::Monthly)
        }
    }

    // ---- Blocking wrappers ----

    /// Blocking variant of [`constituents`][Self::constituents].
    pub fn constituents_blocking(
        &self,
        index: &str,
        ym: impl IntoYearMonth,
    ) -> Result<Vec<Constituent>> {
        let ym = ym.into_year_month()?;
        let index = index.to_string();
        block(self.constituents(&index, ym))
    }

    /// Blocking variant of [`sp500`][Self::sp500].
    pub fn sp500_blocking(&self, ym: impl IntoYearMonth) -> Result<Vec<Constituent>> {
        let ym = ym.into_year_month()?;
        block(self.sp500(ym))
    }

    /// Blocking variant of [`latest`][Self::latest].
    pub fn latest_blocking(&self, id: IndexId) -> Result<DailySnapshot> {
        block(self.latest(id))
    }

    /// Blocking variant of [`sp500_latest`][Self::sp500_latest].
    pub fn sp500_latest_blocking(&self) -> Result<Vec<Constituent>> {
        block(self.sp500_latest())
    }

    /// Blocking variant of [`ndx`][Self::ndx].
    pub fn ndx_blocking(&self, ym: impl IntoYearMonth) -> Result<Vec<Constituent>> {
        let ym = ym.into_year_month()?;
        block(self.ndx(ym))
    }

    /// Blocking variant of [`ndx_latest`][Self::ndx_latest].
    pub fn ndx_latest_blocking(&self) -> Result<Vec<Constituent>> {
        block(self.ndx_latest())
    }

    /// Blocking variant of [`dji`][Self::dji].
    pub fn dji_blocking(&self, ym: impl IntoYearMonth) -> Result<Vec<Constituent>> {
        let ym = ym.into_year_month()?;
        block(self.dji(ym))
    }

    /// Blocking variant of [`dji_latest`][Self::dji_latest].
    pub fn dji_latest_blocking(&self) -> Result<Vec<Constituent>> {
        block(self.dji_latest())
    }

    // ---- Internal ----

    /// The key used for cache and remote URL, of shape `sp500/sp500-YYYY-MM`.
    fn key_for(id: IndexId, ym: YearMonth) -> String {
        format!("{id}/{id}-{ym}")
    }

    async fn load_month(&self, id: IndexId, ym: YearMonth) -> Result<Vec<Constituent>> {
        let key = Self::key_for(id, ym);
        let bytes = match self.fetcher.fetch(&key).await {
            Ok(b) => b,
            Err(Error::Other(msg)) if msg.contains("404") => {
                return Err(Error::SnapshotNotFound {
                    index: id.to_string(),
                    year_month: ym.to_string(),
                });
            }
            Err(e) => return Err(e),
        };
        let tmp = write_bytes_to_tempfile(&bytes)?;
        read_month(tmp.path())
    }

    async fn range(
        &self,
        id: IndexId,
        start: impl IntoYearMonth,
        end: impl IntoYearMonth,
    ) -> Result<Vec<IndexSnapshot>> {
        let start = start.into_year_month()?;
        let end = end.into_year_month()?;
        if start > end {
            return Err(Error::Other(format!("range: start {start} > end {end}")));
        }
        let months: Vec<YearMonth> = start.iter_to(end).collect();
        let fetches = months
            .iter()
            .map(|&ym| async move {
                let res = self.load_month(id, ym).await;
                (ym, res)
            })
            .collect::<Vec<_>>();
        // Sequential-ish but bounded by parallelism of try_join_all. SEC rate
        // limit is generous enough, and fetches land in the cache anyway.
        let pairs = try_join_all(fetches.into_iter().map(|f| async move {
            let (ym, res) = f.await;
            let snap: IndexSnapshot = match res {
                Ok(cs) => IndexSnapshot {
                    index: id,
                    year_month: ym,
                    constituents: cs,
                },
                Err(e) => {
                    // Treat missing months as "gap" rather than fatal.
                    tracing::debug!(?id, %ym, error = %e, "missing snapshot, skipping");
                    IndexSnapshot {
                        index: id,
                        year_month: ym,
                        constituents: Vec::new(),
                    }
                }
            };
            Ok::<IndexSnapshot, Error>(snap)
        }))
        .await?;
        let mut out: Vec<IndexSnapshot> = pairs
            .into_iter()
            .filter(|s| !s.constituents.is_empty())
            .collect();
        out.sort_by_key(|s| s.year_month);
        Ok(out)
    }
}

impl Default for Indexkit {
    fn default() -> Self {
        Self::new()
    }
}

/// One day's rows as a [`DailySnapshot`], heaviest first.
fn day_snapshot(id: IndexId, date: NaiveDate, mut rows: Vec<Constituent>) -> DailySnapshot {
    rows.sort_by(|a, b| {
        b.weight
            .partial_cmp(&a.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // All rows on the same date should share a source (coalesce runs at
    // write time). Take whichever source the first row reports.
    let source = rows
        .first()
        .map(|r| r.source.clone())
        .unwrap_or(crate::types::DataSource::SecNport);
    DailySnapshot {
        index: id,
        date,
        constituents: rows,
        source,
    }
}

fn block<F: std::future::Future<Output = Result<T>>, T>(fut: F) -> Result<T> {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| handle.block_on(fut)),
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(Error::Io)?;
            rt.block_on(fut)
        }
    }
}

fn write_bytes_to_tempfile(bytes: &bytes::Bytes) -> Result<tempfile::NamedTempFile> {
    use std::io::Write;
    let mut tmp = tempfile::NamedTempFile::new()?;
    tmp.write_all(bytes)?;
    tmp.flush()?;
    Ok(tmp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_for_format() {
        let ym = YearMonth::new(2024, 1).unwrap();
        let k = Indexkit::key_for(IndexId::Sp500, ym);
        assert_eq!(k, "sp500/sp500-2024-01");
    }

    fn row(ticker: &str, as_of: NaiveDate) -> Constituent {
        Constituent {
            ticker: Some(ticker.into()),
            name: ticker.into(),
            cusip: String::new(),
            lei: None,
            shares: 1.0,
            market_value_usd: 1.0,
            weight: 0.01,
            issuer_cik: None,
            sector: None,
            as_of,
            source: crate::types::DataSource::SpdrCdn,
        }
    }

    /// A mock origin serving `rows` as `id`'s parquet for `ym`, and a client
    /// pointed at it with no mirror and an empty cache.
    async fn serve_month(
        server: &wiremock::MockServer,
        id: IndexId,
        ym: YearMonth,
        rows: &[Constituent],
    ) {
        let dir = tempfile::TempDir::new().unwrap();
        crate::parquet_io::write_month(dir.path(), id.as_str(), &ym.to_string(), rows).unwrap();
        let bytes = std::fs::read(
            dir.path()
                .join(format!("{}.parquet", Indexkit::key_for(id, ym))),
        )
        .unwrap();
        wiremock::Mock::given(wiremock::matchers::path(format!(
            "/{}.parquet",
            Indexkit::key_for(id, ym)
        )))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(bytes))
        .mount(server)
        .await;
    }

    fn client_for(server: &wiremock::MockServer, cache: &tempfile::TempDir) -> Indexkit {
        Indexkit::new()
            .with_base_url(server.uri())
            .with_cache_dir(cache.path().to_path_buf())
            .with_mirror_url(None)
    }

    /// A month holds every day fetched in it; a member that left mid-month
    /// is on the earlier days only and must not come back as current.
    #[tokio::test]
    async fn latest_returns_only_the_newest_day() {
        let server = wiremock::MockServer::start().await;
        let ym = YearMonth::current_utc();
        let day = |d| NaiveDate::from_ymd_opt(ym.year(), ym.month(), d).unwrap();
        serve_month(
            &server,
            IndexId::Dji,
            ym,
            &[
                row("AAA", day(1)),
                row("LEFT", day(1)),
                row("AAA", day(2)),
                row("NEW", day(2)),
            ],
        )
        .await;
        let cache = tempfile::TempDir::new().unwrap();
        let snap = client_for(&server, &cache)
            .latest(IndexId::Dji)
            .await
            .unwrap();
        assert_eq!(snap.date, day(2));
        let mut tickers: Vec<_> = snap
            .constituents
            .iter()
            .filter_map(|c| c.ticker.as_deref())
            .collect();
        tickers.sort_unstable();
        assert_eq!(tickers, ["AAA", "NEW"]);
    }

    /// Before the first fetch of a month there is no file for it yet.
    #[tokio::test]
    async fn latest_walks_back_past_a_missing_month() {
        let server = wiremock::MockServer::start().await;
        let prev = YearMonth::current_utc().prev();
        let d = NaiveDate::from_ymd_opt(prev.year(), prev.month(), 3).unwrap();
        serve_month(&server, IndexId::Rut, prev, &[row("IWM1", d)]).await;
        let cache = tempfile::TempDir::new().unwrap();
        let snap = client_for(&server, &cache)
            .latest(IndexId::Rut)
            .await
            .unwrap();
        assert_eq!(snap.date, d);
        assert_eq!(snap.constituents.len(), 1);
    }

    /// An origin failure is an error, not a reason to answer with an older
    /// month as if it were the newest.
    #[tokio::test]
    async fn latest_does_not_fall_back_to_an_older_month_on_failure() {
        let server = wiremock::MockServer::start().await;
        let ym = YearMonth::current_utc();
        wiremock::Mock::given(wiremock::matchers::path(format!(
            "/{}.parquet",
            Indexkit::key_for(IndexId::Sp400, ym)
        )))
        .respond_with(wiremock::ResponseTemplate::new(503))
        .mount(&server)
        .await;
        let prev = ym.prev();
        let d = NaiveDate::from_ymd_opt(prev.year(), prev.month(), 3).unwrap();
        serve_month(&server, IndexId::Sp400, prev, &[row("OLD", d)]).await;
        let cache = tempfile::TempDir::new().unwrap();
        let res = client_for(&server, &cache).latest(IndexId::Sp400).await;
        assert!(res.is_err(), "served an older month instead: {res:?}");
    }

    #[test]
    fn key_for_each_index() {
        let ym = YearMonth::new(2020, 12).unwrap();
        for id in IndexId::ALL {
            let k = Indexkit::key_for(id, ym);
            assert!(k.starts_with(id.as_str()));
            assert!(k.ends_with("-2020-12"));
        }
    }
}
