//! `indexkit-cli` -- SEC EDGAR N-PORT + sponsor CDN + Wayback + GitHub OSS
//! backfill + append + inspect.
//!
//! # Commands
//!
//! ```text
//! indexkit-cli backfill                              # SEC N-PORT, all indices
//! indexkit-cli backfill --index sp500                # one index
//! indexkit-cli backfill --index ndx --start 2023-01  # from a given month
//! indexkit-cli daily-fetch --accept-sponsor-tos      # live sponsor CDN (all)
//! indexkit-cli daily-fetch --index sp500 --accept-sponsor-tos
//! indexkit-cli wayback-backfill --index sp500        # IA snapshots, one index
//! indexkit-cli github-backfill                       # all 3 OSS mirrors (fja05680, yfiua, hanshof)
//! indexkit-cli github-backfill --source fja05680     # just one OSS source
//! indexkit-cli nightly-append                        # SEC latest-month append
//! indexkit-cli get sp500 --month 2024-01             # read constituents
//! indexkit-cli manifest                              # regenerate manifest.json
//! indexkit-cli cik-map                               # write cik-map.json
//! ```

use anyhow::{bail, Context, Result};
use chrono::{Datelike, NaiveDate, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use indexkit::cik::{all_entries, entry_for, CikEntry};
use indexkit::coalesce::coalesce;
use indexkit::github_mirror::{
    fetch_fja05680_sp500, fetch_hanshof_sp500, fetch_yfiua_full, forward_fill,
    tickers_to_constituents,
};
use indexkit::nport::holdings_to_constituents;
use indexkit::parquet_io::{read_month, write_month};
use indexkit::sec::SecClient;
use indexkit::sponsor::{parse_invesco_csv, parse_ishares_csv, sponsor_url, SponsorClient};
use indexkit::types::DataSource;
use indexkit::wayback::WaybackClient;
use indexkit::{Constituent, IndexId, YearMonth};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tracing_subscriber::{fmt, EnvFilter};

// ---- CLI ----

#[derive(Parser, Debug)]
#[command(
    name = "indexkit-cli",
    about = "Manage indexkit bundled-parquet data -- SEC N-PORT + sponsor CDN + Wayback",
    version,
    propagate_version = true
)]
struct Cli {
    /// Path to the data directory (default: `<repo-root>/data/`).
    /// Override with $INDEXKIT_DATA_DIR or this flag.
    #[arg(long, env = "INDEXKIT_DATA_DIR", global = true)]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Backfill historical N-PORT filings (monthly baseline).
    Backfill {
        /// Restrict to one index id (sp500, sp400, sp600, ndx, dji).
        #[arg(long)]
        index: Option<String>,

        /// First month to keep (YYYY-MM). Default: 2019-11 (N-PORT public start).
        #[arg(long)]
        start: Option<String>,
    },

    /// Fetch live sponsor-CDN holdings for the current day and merge into
    /// the current month's parquet.
    ///
    /// Each sponsor's terms of service should be reviewed before running.
    /// Pass `--accept-sponsor-tos` to confirm you have done so.
    DailyFetch {
        #[arg(long)]
        index: Option<String>,
        #[arg(long, default_value_t = false)]
        accept_sponsor_tos: bool,
    },

    /// Fill in historical days from the Wayback Machine CDX + snapshot
    /// API. Merges with existing N-PORT baseline where it exists.
    WaybackBackfill {
        #[arg(long)]
        index: Option<String>,
        /// From date YYYY-MM-DD (default 2019-11-01).
        #[arg(long)]
        from: Option<String>,
        /// To date YYYY-MM-DD (default today).
        #[arg(long)]
        to: Option<String>,
    },

    /// Ingest ticker-only historical constituents from the three free OSS
    /// GitHub mirrors: fja05680/sp500 (MIT), yfiua/index-constituents
    /// (Apache-2.0), hanshof/sp500_constituents (MIT).
    ///
    /// All rows are tagged with the appropriate `DataSource::Github*`
    /// variant. Existing N-PORT / CDN / Wayback data is preserved; the
    /// coalesce layer dedupes by `(identity, as_of)` on parquet write.
    GithubBackfill {
        /// Restrict to one OSS source. Default: all three.
        #[arg(long, value_enum)]
        source: Option<GithubSource>,
    },

    /// Fetch any new N-PORT filings since last run and append.
    NightlyAppend,

    /// Read a month from bundled parquet and print to stdout.
    Get {
        index: String,
        /// Month (YYYY-MM). Omit for the latest month in the data directory.
        #[arg(long)]
        month: Option<String>,
    },

    /// Generate `data/manifest.json` with SHA-256 digests for every parquet.
    Manifest,

    /// Write `data/cik-map.json` -- committed for non-Rust consumers.
    CikMap,
}

/// Which OSS GitHub mirror to ingest.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum GithubSource {
    /// fja05680/sp500 -- S&P 500 daily change-rows, 1996 -> present.
    Fja05680,
    /// yfiua/index-constituents -- S&P 500 / Nasdaq-100 / Dow Jones
    /// monthly snapshots, ~2018 -> present.
    Yfiua,
    /// hanshof/sp500_constituents -- S&P 500 daily change-rows, 1996 ->
    /// present. Cross-check layer.
    Hanshof,
}

// ---- Entry point ----

#[tokio::main]
async fn main() -> Result<()> {
    fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let data_dir = cli.data_dir.unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("data")
    });
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data dir {}", data_dir.display()))?;

    match cli.cmd {
        Command::Backfill { index, start } => {
            cmd_backfill(&data_dir, index.as_deref(), start.as_deref()).await?
        }
        Command::DailyFetch {
            index,
            accept_sponsor_tos,
        } => cmd_daily_fetch(&data_dir, index.as_deref(), accept_sponsor_tos).await?,
        Command::WaybackBackfill { index, from, to } => {
            cmd_wayback_backfill(&data_dir, index.as_deref(), from.as_deref(), to.as_deref())
                .await?
        }
        Command::GithubBackfill { source } => cmd_github_backfill(&data_dir, source).await?,
        Command::NightlyAppend => cmd_nightly_append(&data_dir).await?,
        Command::Get { index, month } => cmd_get(&data_dir, &index, month.as_deref())?,
        Command::Manifest => cmd_manifest(&data_dir)?,
        Command::CikMap => cmd_cik_map(&data_dir)?,
    }
    Ok(())
}

// ---- helpers ----

fn parse_ym(s: &str) -> Result<YearMonth> {
    <YearMonth as std::str::FromStr>::from_str(s).with_context(|| format!("parsing year-month {s}"))
}

fn period_to_year_month(period: &str) -> Option<YearMonth> {
    if period.len() < 7 {
        return None;
    }
    <YearMonth as std::str::FromStr>::from_str(&period[..7]).ok()
}

fn select_indices(filter: Option<&str>) -> Result<Vec<IndexId>> {
    match filter {
        Some(s) => {
            Ok(vec![IndexId::from_str_id(s).ok_or_else(|| {
                anyhow::anyhow!("unknown index id: {s:?}")
            })?])
        }
        None => Ok(IndexId::ALL.to_vec()),
    }
}

fn existing_rows(data_dir: &Path, index: &str, ym: YearMonth) -> Vec<Constituent> {
    let path = data_dir.join(index).join(format!("{index}-{ym}.parquet"));
    if !path.exists() {
        return Vec::new();
    }
    read_month(&path).unwrap_or_default()
}

fn existing_months(dir: &Path) -> std::collections::BTreeSet<YearMonth> {
    let mut out = std::collections::BTreeSet::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if let Some(rest) = name.strip_suffix(".parquet") {
            // match `{idx}-YYYY-MM`, i.e. take the last 7 chars.
            if rest.len() >= 7 {
                let ym_str = &rest[rest.len() - 7..];
                if let Ok(ym) = <YearMonth as std::str::FromStr>::from_str(ym_str) {
                    out.insert(ym);
                }
            }
        }
    }
    out
}

// ---- SEC N-PORT backfill ----

async fn cmd_backfill(
    data_dir: &Path,
    index_filter: Option<&str>,
    start: Option<&str>,
) -> Result<()> {
    let start_ym = match start {
        Some(s) => parse_ym(s)?,
        None => YearMonth::new(2019, 11).unwrap(),
    };
    let indices = select_indices(index_filter)?;
    let sec = SecClient::new()?;
    let mut had_errors = false;

    for idx in indices {
        match backfill_one_nport(&sec, data_dir, idx, start_ym).await {
            Ok((w, s)) => {
                println!("{idx}: wrote {w} months, skipped {s} duplicates");
            }
            Err(e) => {
                tracing::error!(%idx, "backfill failed: {e}");
                had_errors = true;
            }
        }
    }
    if had_errors {
        bail!("one or more index backfills failed (see logs above)");
    }
    Ok(())
}

async fn backfill_one_nport(
    sec: &SecClient,
    data_dir: &Path,
    idx: IndexId,
    start_ym: YearMonth,
) -> Result<(usize, usize)> {
    let entry = entry_for(idx);
    tracing::info!(%idx, ticker=%entry.ticker, cik=%entry.trust_cik, series=?entry.series_id, "backfill start");

    let pairs = sec
        .filings_for_series(&entry)
        .await
        .with_context(|| format!("filings_for_series {idx}"))?;
    tracing::info!(%idx, count = pairs.len(), "filings matching series");

    let mut written = 0usize;
    let mut skipped = 0usize;
    let mut seen_months: std::collections::BTreeSet<YearMonth> = Default::default();

    for (_fref, nport) in pairs {
        let Some(period_end) = &nport.header.reporting_period_end else {
            continue;
        };
        let Some(ym) = period_to_year_month(period_end) else {
            continue;
        };
        if ym < start_ym {
            continue;
        }
        if !seen_months.insert(ym) {
            skipped += 1;
            continue;
        }
        let new_rows = holdings_to_constituents(&nport);
        if new_rows.is_empty() {
            tracing::warn!(%idx, %ym, "zero equity holdings -- skipping");
            continue;
        }
        // Merge with any existing rows (from a previous CDN or Wayback run).
        let old_rows = existing_rows(data_dir, idx.as_str(), ym);
        let merged = coalesce(vec![old_rows, new_rows]);
        write_month(data_dir, idx.as_str(), &ym.to_string(), &merged)
            .with_context(|| format!("writing {idx} {ym}"))?;
        written += 1;
    }
    Ok((written, skipped))
}

// ---- daily sponsor CDN ----

async fn cmd_daily_fetch(
    data_dir: &Path,
    index_filter: Option<&str>,
    accept_tos: bool,
) -> Result<()> {
    if !accept_tos {
        bail!(
            "sponsor CDN fetches may be constrained by each sponsor's terms \
             of service. Re-run with `--accept-sponsor-tos` after review."
        );
    }
    let indices = select_indices(index_filter)?;
    let client = SponsorClient::new()?;
    let today = Utc::now().date_naive();
    let ym = YearMonth::new(today.year(), today.month()).unwrap();

    for idx in indices {
        match fetch_sponsor_one(&client, data_dir, idx, today, ym).await {
            Ok(n) => println!("{idx}: appended {n} rows from sponsor CDN"),
            Err(e) => tracing::warn!(%idx, "sponsor fetch failed: {e}"),
        }
    }
    Ok(())
}

async fn fetch_sponsor_one(
    client: &SponsorClient,
    data_dir: &Path,
    idx: IndexId,
    today: NaiveDate,
    ym: YearMonth,
) -> Result<usize> {
    let (source, body) = client.fetch_today(idx).await?;
    let text = std::str::from_utf8(&body)
        .map_err(|e| anyhow::anyhow!("sponsor response not UTF-8: {e}"))?;

    let rows: Vec<Constituent> = match source {
        DataSource::IsharesCdn => parse_ishares_csv(text, today, source.clone())?,
        DataSource::InvescoCdn => parse_invesco_csv(text, today)?,
        DataSource::SpdrCdn => {
            // State Street publishes an XLSX; we attempt CSV first (their
            // CSV endpoint exists for some products). If decoding fails,
            // we cannot parse .xlsx in v1.0 -- this is a documented gap.
            tracing::warn!(
                "SPDR sponsor response is XLSX; CSV parser cannot read it. \
                 Wayback backfill will still work for historical days."
            );
            return Err(anyhow::anyhow!("SPDR XLSX not parseable in v1.0"));
        }
        _ => return Err(anyhow::anyhow!("unexpected source {source:?}")),
    };
    if rows.is_empty() {
        return Err(anyhow::anyhow!(
            "sponsor CSV parsed to zero rows (format changed?)"
        ));
    }

    let old = existing_rows(data_dir, idx.as_str(), ym);
    let merged = coalesce(vec![old, rows.clone()]);
    write_month(data_dir, idx.as_str(), &ym.to_string(), &merged)?;
    Ok(rows.len())
}

// ---- Wayback backfill ----

async fn cmd_wayback_backfill(
    data_dir: &Path,
    index_filter: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<()> {
    let from_s = from.unwrap_or("2019-11-01");
    let to_s = to
        .map(str::to_string)
        .unwrap_or_else(|| Utc::now().date_naive().format("%Y-%m-%d").to_string());
    let from_nd = NaiveDate::parse_from_str(from_s, "%Y-%m-%d")
        .with_context(|| format!("parsing --from {from_s}"))?;
    let to_nd = NaiveDate::parse_from_str(&to_s, "%Y-%m-%d")
        .with_context(|| format!("parsing --to {to_s}"))?;

    let indices = select_indices(index_filter)?;
    let wb = WaybackClient::new()?;
    let from_yyyymmdd = from_nd.format("%Y%m%d").to_string();
    let to_yyyymmdd = to_nd.format("%Y%m%d").to_string();

    for idx in indices {
        let Some((source, _ticker, url)) = sponsor_url(idx) else {
            continue;
        };
        println!(
            "{idx}: listing Wayback snapshots {} -> {} for {}",
            from_yyyymmdd, to_yyyymmdd, url
        );
        let matches = match wb.list(url, &from_yyyymmdd, &to_yyyymmdd).await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(%idx, "CDX fetch failed: {e}");
                continue;
            }
        };
        println!("{idx}: {} snapshots found", matches.len());

        let mut by_month: BTreeMap<YearMonth, Vec<Constituent>> = BTreeMap::new();
        for m in matches {
            let Some(d) = m.date() else { continue };
            let ym = YearMonth::new(d.year(), d.month()).unwrap();
            let body = match wb.fetch(&m).await {
                Ok(b) => b,
                Err(e) => {
                    tracing::debug!("skip snapshot {}: {e}", m.snapshot_url());
                    continue;
                }
            };
            let text = match std::str::from_utf8(&body) {
                Ok(t) => t,
                Err(_) => continue,
            };
            let tag = DataSource::Wayback(m.timestamp[..8].to_string());
            let rows = match source {
                DataSource::IsharesCdn => match parse_ishares_csv(text, d, tag) {
                    Ok(r) => r,
                    Err(_) => continue,
                },
                DataSource::InvescoCdn => match parse_invesco_csv(text, d) {
                    Ok(mut r) => {
                        for row in &mut r {
                            row.source = DataSource::Wayback(m.timestamp[..8].to_string());
                        }
                        r
                    }
                    Err(_) => continue,
                },
                DataSource::SpdrCdn => {
                    // Wayback snapshots of SPDR .xlsx are not parseable with
                    // the CSV parser. Skip.
                    continue;
                }
                _ => continue,
            };
            if !rows.is_empty() {
                by_month.entry(ym).or_default().extend(rows);
            }
        }

        // Merge per-month.
        for (ym, rows) in by_month {
            let old = existing_rows(data_dir, idx.as_str(), ym);
            let merged = coalesce(vec![old, rows]);
            if let Err(e) = write_month(data_dir, idx.as_str(), &ym.to_string(), &merged) {
                tracing::warn!(%idx, %ym, "write failed: {e}");
                continue;
            }
            println!("{idx} {ym}: merged, {} rows", merged.len());
        }
    }
    Ok(())
}

// ---- github-backfill (OSS mirror ingestion) ----

async fn cmd_github_backfill(data_dir: &Path, source: Option<GithubSource>) -> Result<()> {
    let mut had_errors = false;
    let run = |src: GithubSource| match src {
        GithubSource::Fja05680 => "fja05680",
        GithubSource::Yfiua => "yfiua",
        GithubSource::Hanshof => "hanshof",
    };
    let sources: Vec<GithubSource> = match source {
        Some(s) => vec![s],
        None => vec![
            GithubSource::Fja05680,
            GithubSource::Yfiua,
            GithubSource::Hanshof,
        ],
    };

    // Cross-check bucket: collect per-date ticker-sets from fja05680 and
    // hanshof so we can log disagreements after both have loaded.
    let mut fja_by_date: BTreeMap<NaiveDate, std::collections::BTreeSet<String>> = BTreeMap::new();
    let mut hanshof_by_date: BTreeMap<NaiveDate, std::collections::BTreeSet<String>> =
        BTreeMap::new();

    for src in sources {
        let tag = run(src);
        println!("github-backfill: ingesting {tag}");
        let res = match src {
            GithubSource::Fja05680 => {
                ingest_daily_sp500(data_dir, DataSource::GithubFja05680, &mut fja_by_date).await
            }
            GithubSource::Yfiua => ingest_yfiua_all(data_dir).await,
            GithubSource::Hanshof => {
                ingest_daily_sp500(data_dir, DataSource::GithubHanshof, &mut hanshof_by_date).await
            }
        };
        match res {
            Ok(count) => println!("github-backfill: {tag} wrote {count} month files"),
            Err(e) => {
                tracing::error!("github-backfill {tag} failed: {e}");
                had_errors = true;
            }
        }
    }

    // Cross-check log: fja05680 vs hanshof disagreement on same date.
    if !fja_by_date.is_empty() && !hanshof_by_date.is_empty() {
        let mut disagree = 0usize;
        for (date, fja_set) in &fja_by_date {
            let Some(hanshof_set) = hanshof_by_date.get(date) else {
                continue;
            };
            if fja_set != hanshof_set {
                disagree += 1;
                // Log deterministic diff summary but cap the per-date spam.
                if disagree <= 10 {
                    let only_fja: Vec<_> = fja_set.difference(hanshof_set).take(5).collect();
                    let only_hanshof: Vec<_> = hanshof_set.difference(fja_set).take(5).collect();
                    tracing::warn!(
                        "github-backfill cross-check {date}: fja-only={:?} hanshof-only={:?}",
                        only_fja,
                        only_hanshof,
                    );
                }
            }
        }
        if disagree > 0 {
            println!(
                "github-backfill: cross-check found {disagree} dates where \
                 fja05680 and hanshof disagree (see warnings above). \
                 Higher-priority source wins at coalesce."
            );
        }
    }

    if had_errors {
        bail!("one or more github-backfill sources failed (see logs above)");
    }
    Ok(())
}

/// Ingest a daily change-row SP500 source (fja05680 or hanshof).
///
/// Forward-fills change-rows into per-calendar-day rows, groups by
/// year-month, and writes each month parquet merged with existing rows.
async fn ingest_daily_sp500(
    data_dir: &Path,
    source: DataSource,
    record_by_date: &mut BTreeMap<NaiveDate, std::collections::BTreeSet<String>>,
) -> Result<usize> {
    let changes = match source {
        DataSource::GithubFja05680 => fetch_fja05680_sp500()
            .await
            .context("fetch fja05680/sp500")?,
        DataSource::GithubHanshof => fetch_hanshof_sp500()
            .await
            .context("fetch hanshof/sp500_constituents")?,
        _ => bail!("ingest_daily_sp500: unsupported source {source:?}"),
    };
    if changes.is_empty() {
        bail!("upstream returned zero change-rows");
    }
    let daily = forward_fill(&changes);
    tracing::info!(
        "{:?}: {} change-rows expanded to {} calendar-day rows",
        source,
        changes.len(),
        daily.len()
    );
    let idx = IndexId::Sp500;
    let mut by_month: BTreeMap<YearMonth, Vec<Constituent>> = BTreeMap::new();
    for (date, tickers) in daily {
        record_by_date.insert(date, tickers.iter().cloned().collect());
        let ym = YearMonth::new(date.year(), date.month()).unwrap();
        let rows = tickers_to_constituents(&tickers, date, source.clone());
        by_month.entry(ym).or_default().extend(rows);
    }
    let months = by_month.len();
    for (ym, rows) in by_month {
        let old = existing_rows(data_dir, idx.as_str(), ym);
        let merged = coalesce(vec![old, rows]);
        write_month(data_dir, idx.as_str(), &ym.to_string(), &merged)
            .with_context(|| format!("writing {idx} {ym}"))?;
    }
    Ok(months)
}

/// Ingest every available yfiua month for every yfiua-supported index.
async fn ingest_yfiua_all(data_dir: &Path) -> Result<usize> {
    let indices = [IndexId::Sp500, IndexId::Ndx, IndexId::Dji];
    let mut total = 0usize;
    for idx in indices {
        let pairs = match fetch_yfiua_full(idx, None, None).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(%idx, "yfiua fetch failed: {e}");
                continue;
            }
        };
        tracing::info!(%idx, count = pairs.len(), "yfiua months fetched");
        for (ym, tickers) in pairs {
            if tickers.is_empty() {
                continue;
            }
            // yfiua's monthly snapshot is stamped at the 15th of the month --
            // a deterministic synthetic mid-month date for the parquet `as_of`.
            let day = yfiua_mid_month(ym);
            let source = DataSource::GithubYfiua { month: ym };
            let rows = tickers_to_constituents(&tickers, day, source);
            let old = existing_rows(data_dir, idx.as_str(), ym);
            let merged = coalesce(vec![old, rows]);
            write_month(data_dir, idx.as_str(), &ym.to_string(), &merged)
                .with_context(|| format!("writing {idx} {ym} (yfiua)"))?;
            total += 1;
        }
    }
    Ok(total)
}

/// Deterministic mid-month date for yfiua's monthly stamp.
fn yfiua_mid_month(ym: YearMonth) -> NaiveDate {
    NaiveDate::from_ymd_opt(ym.year(), ym.month(), 15)
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(ym.year(), ym.month(), 1).unwrap())
}

// ---- nightly-append (SEC N-PORT only) ----

async fn cmd_nightly_append(data_dir: &Path) -> Result<()> {
    let sec = SecClient::new()?;
    let mut any_written = false;
    for idx in IndexId::ALL {
        match nightly_append_one(&sec, data_dir, idx).await {
            Ok(true) => any_written = true,
            Ok(false) => tracing::info!(%idx, "no new months to append"),
            Err(e) => tracing::warn!(%idx, "nightly append failed: {e}"),
        }
    }
    if !any_written {
        println!("No new data. Nothing to commit.");
    }
    Ok(())
}

async fn nightly_append_one(sec: &SecClient, data_dir: &Path, idx: IndexId) -> Result<bool> {
    let entry = entry_for(idx);
    let dir = data_dir.join(entry.index.clone());
    let have: std::collections::BTreeSet<YearMonth> = existing_months(&dir);

    // Use the fast search-by-series path; take the 5 newest and append any
    // months we don't already have.
    let pairs = sec.filings_for_series(&entry).await?;
    let recent: Vec<_> = pairs.into_iter().take(5).collect();

    let mut wrote = false;
    for (_f, nport) in recent {
        let Some(period_end) = &nport.header.reporting_period_end else {
            continue;
        };
        let Some(ym) = period_to_year_month(period_end) else {
            continue;
        };
        if have.contains(&ym) {
            continue;
        }
        let new_rows = holdings_to_constituents(&nport);
        if new_rows.is_empty() {
            continue;
        }
        let old = existing_rows(data_dir, idx.as_str(), ym);
        let merged = coalesce(vec![old, new_rows]);
        write_month(data_dir, idx.as_str(), &ym.to_string(), &merged)
            .with_context(|| format!("append {idx} {ym}"))?;
        wrote = true;
    }
    Ok(wrote)
}

// ---- get ----

fn cmd_get(data_dir: &Path, index: &str, month: Option<&str>) -> Result<()> {
    let id =
        IndexId::from_str_id(index).ok_or_else(|| anyhow::anyhow!("unknown index: {index}"))?;
    let dir = data_dir.join(id.as_str());
    let ym = match month {
        Some(m) => parse_ym(m)?,
        None => existing_months(&dir)
            .iter()
            .next_back()
            .copied()
            .ok_or_else(|| anyhow::anyhow!("no data for {index} in {}", dir.display()))?,
    };
    let path = dir.join(format!("{}-{}.parquet", id.as_str(), ym));
    if !path.exists() {
        bail!("{} not found -- run backfill first", path.display());
    }
    let cs = read_month(&path)?;
    println!(
        "{id} -- {ym} -- {} rows (may span multiple dates)",
        cs.len()
    );
    println!(
        "{:<8} {:<40} {:<12} {:<10} {:<24} {:>11}",
        "Ticker", "Name", "CUSIP", "Date", "Source", "Weight %"
    );
    println!("{}", "-".repeat(112));
    for c in cs.iter().take(30) {
        let weight_str = if c.weight.is_finite() {
            format!("{:>10.4}%", c.weight * 100.0)
        } else {
            format!("{:>11}", "-")
        };
        println!(
            "{:<8} {:<40} {:<12} {:<10} {:<24} {}",
            c.ticker.as_deref().unwrap_or("-"),
            truncate(&c.name, 40),
            if c.cusip.is_empty() { "-" } else { &c.cusip },
            c.as_of,
            c.source.tag(),
            weight_str,
        );
    }
    if cs.len() > 30 {
        println!("... {} more not shown", cs.len() - 30);
    }
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}...", &s[..n.saturating_sub(3)])
    }
}

// ---- manifest ----

fn cmd_manifest(data_dir: &Path) -> Result<()> {
    let mut entries: BTreeMap<String, String> = BTreeMap::new();
    walk_parquets(data_dir, data_dir, &mut entries)?;
    let manifest =
        serde_json::to_string_pretty(&entries).context("serializing manifest to JSON")?;
    let manifest_path = data_dir.join("manifest.json");
    std::fs::write(&manifest_path, manifest)
        .with_context(|| format!("writing {}", manifest_path.display()))?;
    println!(
        "Wrote manifest with {} entries -> {}",
        entries.len(),
        manifest_path.display()
    );
    Ok(())
}

fn walk_parquets(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) -> Result<()> {
    for e in std::fs::read_dir(dir)
        .with_context(|| format!("reading dir {}", dir.display()))?
        .flatten()
    {
        let path = e.path();
        if path.is_dir() {
            walk_parquets(root, &path, out)?;
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".parquet") {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let mut h = Sha256::new();
        h.update(&bytes);
        let digest = h.finalize();
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        out.insert(rel, format!("sha256:{hex}"));
    }
    Ok(())
}

// ---- cik-map write ----

fn cmd_cik_map(data_dir: &Path) -> Result<()> {
    let mut out: BTreeMap<String, CikEntry> = BTreeMap::new();
    for e in all_entries() {
        out.insert(e.index.clone(), e);
    }
    let path = data_dir.join("cik-map.json");
    let json = serde_json::to_string_pretty(&out)?;
    std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
    println!("Wrote {}", path.display());
    Ok(())
}
