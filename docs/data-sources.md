# Data sources

indexkit layers six independent data sources, each with its own license,
coverage window, and field completeness. Every row in the bundled parquet
files carries a `source` column naming the upstream. Coalesce priority
(see [architecture.md](architecture.md)) breaks ties when multiple
sources cover the same `(identity, date)` key.

## Tier 5 -- ETF sponsor CDNs

**Publisher:** iShares (IVV, IJH, IJR), Invesco (QQQ), State Street (DIA).

**Coverage:** forward-going daily, T+1 publication.

**Licence posture:** each sponsor's terms of service apply. indexkit
gates the live fetcher behind `--accept-sponsor-tos`.

**Fields:** full -- ticker, name, CUSIP, shares, market value, weight.

See `crates/indexkit/src/sponsor.rs` for URL constants and the CSV
parsers.

## Tier 4 -- fja05680/sp500

**Publisher:** [Farrell J. Aultman](https://github.com/fja05680)

**Repo:** https://github.com/fja05680/sp500

**Licence:** MIT (see `data/licenses/fja05680-LICENSE`).

**Coverage:** 1996-01-02 to present (last refresh: 2026-01 in the v1.0.1
release window). Daily change-row format -- one row per date the S&P 500
composition changed, not per trading day. indexkit's
`github_mirror::forward_fill` carries the composition through each
calendar date between change rows.

**Raw URL (dated):**
`https://raw.githubusercontent.com/fja05680/sp500/master/S%26P%20500%20Historical%20Components%20%26%20Changes(MM-DD-YYYY).csv`

Falls back to the legacy filename when the dated file is not found:
`https://raw.githubusercontent.com/fja05680/sp500/master/S%26P%20500%20Historical%20Components%20%26%20Changes.csv`

**Schema:** two-column CSV, `date,tickers`, where `tickers` is a
double-quoted comma-separated list. Removed tickers carry a `-YYYYMM`
suffix (e.g. `AAL-199702`) encoding the month the ticker left the
index; indexkit strips these suffixes during parsing.

**Fields:** ticker only. `cusip`, `lei`, `shares`, `market_value_usd`
are left at their "unknown" sentinels; `weight` is `f64::NAN`.

**Gotcha:** one ticker may appear twice on the same date with different
suffixes (once "active" with no suffix, once "removed on date X") --
indexkit's strip+dedup layer handles this correctly when building daily
rows.

## Tier 3a -- yfiua/index-constituents

**Publisher:** [yfiua](https://github.com/yfiua)

**Repo:** https://github.com/yfiua/index-constituents

**Licence:** Apache-2.0 (see `data/licenses/yfiua-LICENSE`).

**Coverage:** monthly, ~2018-07 to present for `sp500`. Other indices
(`nasdaq100`, `dowjones`, `dax`, non-US) start later (~2023). yfiua
publishes for months only when the scraping pipeline succeeded; early
years have single-month coverage only.

**Raw URL template:**
`https://raw.githubusercontent.com/yfiua/index-constituents/master/docs/{YYYY}/{MM}/constituents-{CODE}.csv`

Supported codes in indexkit: `sp500`, `nasdaq100`, `dowjones`. yfiua does
NOT publish `sp400` or `sp600`.

**Schema:** two-column CSV, `Symbol,Name`. `Name` may contain quoted
commas for security names like `"Meta Platforms, Inc. Class A"`.

**Fields:** ticker only. Same NaN-weight / empty-cusip convention as
fja05680.

**Stamp date:** indexkit tags each yfiua row with a deterministic
synthetic `as_of` of the 15th of the month (arbitrary mid-month marker)
because the upstream CSV has no intra-month dating.

## Tier 3b -- hanshof/sp500_constituents

**Publisher:** running_error

**Repo:** https://github.com/hanshof/sp500_constituents

**Licence:** MIT (see `data/licenses/hanshof-LICENSE`).

**Coverage:** 1996 to present, daily change-rows. Same shape as
fja05680 but with cleaner tickers (no `-YYYYMM` suffixes).

**Raw URL:**
`https://raw.githubusercontent.com/hanshof/sp500_constituents/master/sp_500_historical_components.csv`

**Role:** cross-check layer for fja05680. If the two sources disagree
on the ticker universe for a given date, `cmd_github_backfill` logs a
warning with a diff summary. The higher-priority source (fja05680, tier
4) wins at coalesce.

## Tier 2 -- Internet Archive Wayback Machine

**Publisher:** Internet Archive (501(c)(3) archival service).

**Coverage:** sparse snapshots of sponsor-CDN URLs from ~2019-11 onward;
roughly 40-60 % of trading days depending on how often the Archive's
crawlers captured each URL.

**Licence posture:** fair-use archival under US law. The CDX + snapshot
APIs are designed for automated access; indexkit inserts rate-limit
sleeps (250 ms CDX, 500 ms snapshot fetch).

**Fields:** inherited from the underlying CDN payload at capture time.

See `crates/indexkit/src/wayback.rs`.

## Tier 1 -- SEC EDGAR N-PORT

**Publisher:** US Securities and Exchange Commission.

**Form:** N-PORT-P (Monthly Portfolio Investments). Rule
[30b1-9 under the Investment Company Act of 1940](https://www.sec.gov/rules-regulations/2016/10/modernization-information-reported-registered-investment-companies).
Investment companies must file Form N-PORT within 60 days after the end
of each month. The third month of each quarter is made publicly
available 60 days after filing (so a June 30 holdings file becomes
public approximately August 29 -- a ~60 day lag). For non-quarter-ending
months the SEC holds the filing for an additional 60 days before
release.

**User-Agent:** SEC mandates a descriptive UA for all `data.sec.gov` and
`www.sec.gov/Archives` requests. The Akamai edge rejects anonymous,
`noreply@`, and library-style `(+url)` UAs. indexkit uses the format
`indexkit <contact-email>`, overridable via `INDEXKIT_SEC_USER_AGENT`.

**Rate limit:** 10 requests / second per IP. indexkit inserts a 120 ms
sleep between requests to stay comfortably under the limit.

### Endpoints

1. **Submissions feed** (list filings for a CIK):
   ```
   https://data.sec.gov/submissions/CIK{10-digit-zero-padded-cik}.json
   ```
   Returns JSON with fields `filings.recent.form`, `filingDate`,
   `accessionNumber`, `reportDate`. For CIKs with more than ~1000 total
   filings, older ones live in paginated archives listed under
   `filings.files[].name`.

2. **Filing document** (XML):
   ```
   https://www.sec.gov/Archives/edgar/data/{cik-no-leading-zeros}/{accession-no-dashes}/primary_doc.xml
   ```
   One XML per filing. Multi-MB for large-universe ETFs (IVV is ~4 MB
   with 500+ holdings).

### Publication schedule

| Reporting month end | Filing deadline (T + 60d) | Public release (T + 90d) |
|---|---|---|
| 2024-06-30 | 2024-08-29 | 2024-09-28 (quarter end, fast release) |
| 2024-07-31 | 2024-09-29 | ~2024-10-29 |
| 2024-08-31 | 2024-10-30 | ~2024-11-29 |
| 2024-09-30 | 2024-11-29 | 2024-12-29 (quarter end) |

So for downstream consumers, N-PORT "latest" is typically 2-3 months
behind real time. indexkit cannot close that gap -- it is imposed by
the SEC for investor-protection reasons. OSS GitHub mirrors (tiers 3-4)
and sponsor-CDN (tier 5) close the recency gap going forward.

### The five index proxies

Each of the five indices indexkit supports is tracked via a single
iconic ETF. The CIK and series ID for each proxy are verified against
live SEC data (see `data/cik-map.json`):

| Index | Ticker | ETF name | Trust CIK | Series ID |
|---|---|---|---|---|
| S&P 500 | IVV | iShares Core S&P 500 ETF | 0001100663 | S000004310 |
| S&P MidCap 400 | IJH | iShares Core S&P Mid-Cap ETF | 0001100663 | S000004307 |
| S&P SmallCap 600 | IJR | iShares Core S&P Small-Cap ETF | 0001100663 | S000004313 |
| Nasdaq-100 | QQQ | Invesco QQQ Trust, Series 1 | 0001067839 | S000101292 |
| DJIA | DIA | SPDR Dow Jones Industrial Average ETF Trust | 0001041130 | *none* (single-series trust) |

iShares Trust (CIK 0001100663) is a multi-series trust hosting ~130
ETFs; indexkit must open each filing's `primary_doc.xml` to match the
target series ID. iShares Trust files hundreds of NPORT-P per month, so
the backfill is I/O-heavy on the SEC side.

QQQ's CIK 0001067839 hosts only Invesco QQQ Trust Series 1, so every
NPORT-P filing for that CIK is for QQQ. Same for DIA
(CIK 0001041130).

### Coverage

N-PORT public release began in Q4 2019 (first data month: October 2019,
first public release: November 2019). Prior to that, ETFs filed Form
N-Q on a quarterly basis with less granular detail. indexkit does NOT
read N-Q -- coverage via N-PORT starts Nov 2019. For pre-2019 coverage,
use tier-4 (fja05680) or tier-3b (hanshof).

## Rate conversion

N-PORT values are already denominated correctly: USD market value,
share counts, and NAV fractions. No rate conversions are needed.

`pctVal` in N-PORT is expressed as a fraction (e.g. 0.073 for a 7.3 %
weight), so indexkit passes it through unchanged into
`Constituent::weight`.

The OSS GitHub mirrors carry no weight data -- `Constituent::weight` is
`f64::NAN` for those rows and `Constituent::weight_opt()` returns
`None`.

## Attribution summary

- SEC EDGAR filings are US federal records in the public domain.
- Wayback snapshots are archival holdings of sponsor pages under fair-
  use doctrine by the Internet Archive.
- fja05680/sp500: MIT, copyright (c) 2019-2020 Farrell J. Aultman.
- yfiua/index-constituents: Apache-2.0.
- hanshof/sp500_constituents: MIT, copyright (c) 2023 running_error.

Verbatim LICENSE files ship in `data/licenses/`. indexkit's own code
and derivative parquet outputs are Apache-2.0.
