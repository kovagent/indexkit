# Architecture

## Data flow

```
    +------------------+    +--------------------------------+
    | SEC EDGAR NPORT  |    |   GitHub Actions (runner)      |
    | primary_doc.xml  |----|  backfill.yml (manual)         |
    | monthly filings  |    |  nightly.yml (cron weekday     |
    +------------------+    |              07:00 UTC)        |
                            +---------------+----------------+
                                            | commit data/{index}/*.parquet
                                            v
                     +-------------------------------------+
                     |  userFRM/indexkit repo              |
                     |  data/sp500/sp500-YYYY-MM.parquet   |
                     |  data/ndx/ndx-YYYY-MM.parquet       |
                     |  ...                                |
                     +-------------------+-----------------+
                                         | raw.githubusercontent.com/.../data/
                                         v
    +-------------------------------------------------------+
    |  indexkit::Indexkit (client lib)                      |
    |  fetch -> ETag check -> SHA-256 verify -> parquet     |
    |  cache: ~/.cache/indexkit/                            |
    +-------------------------------------------------------+
                                         ^
                                         | flat async API
            +----------------------------+--------------------+
            |  user app (e.g. Kairos, an analytics service)   |
            |  client.sp500(ym!(2024, 1)).await?              |
            +-------------------------------------------------+
```

## Crates

### indexkit (lib)

Single library crate. Contains:

- `client` -- `Indexkit` struct with flat async endpoint methods.
- `fetcher` -- `CachedFetcher`: ETag-aware HTTP fetch + disk cache + retry
  + mirror fallback + SHA-256 verification.
- `date` -- `YearMonth` value type, `IntoYearMonth` trait, `ym!` macro.
- `types` -- `Constituent`, `IndexSnapshot`, `IndexId`, `Sector`.
- `nport` -- streaming XML parser for `primary_doc.xml`.
- `parquet_io` -- parquet writer + reader for the monthly snapshot schema.
- `cik` -- static CIK / series map, verified against live SEC data.
- `sec` -- SEC EDGAR submissions + archives API client (used by CLI).
- `error` -- unified error enum.

### indexkit-cli (binary)

All N-PORT ingestion logic lives here. Consumes the `indexkit` lib.

## Cache semantics

Cache directory: `~/.cache/indexkit/` (XDG via the `directories` crate).
Override: `$INDEXKIT_CACHE_DIR`.

```
~/.cache/indexkit/
+- sp500/
|  +- sp500-2024-01.parquet
|  +- sp500-2024-01.parquet.etag
|  +- ...
+- ndx/
+- dji/
+- ...
```

On each `Indexkit` method call the internal `CachedFetcher::fetch` runs
this logic for the relevant month file:

1. **Single-flight**: dedupe concurrent callers for the same key via
   `Arc<OnceCell>`.
2. **ETag**: if the file is cached and an ETag is stored, send
   `If-None-Match`.
3. `304 Not Modified` -> return cached bytes, no download.
4. `2xx` -> write body + ETag, hand off to verifier.
5. **Retry**: 5xx / 429 / connect / timeout retries with exponential
   backoff (250 ms, 750 ms, 2 000 ms). 429 honours `Retry-After`. Max 3
   total attempts.
6. **Mirror fallback**: after primary retries exhaust, try jsDelivr CDN
   mirror. Single attempt, no retry.
7. **Stale fallback**: if all transports fail but a cached file exists,
   warn and return stale bytes.
8. **SHA-256 verify**: if `manifest.json` has an entry for the key, verify
   the downloaded bytes. Mismatch returns `Error::ChecksumMismatch` and
   the bad bytes are NOT written to cache.

## Data format

### Monthly parquet schema

One row per holding:

| Column | Arrow type | Nullable | Description |
|---|---|---|---|
| `ticker` | `Utf8` | yes | Always null in v1.0 (N-PORT has no ticker) |
| `name` | `Utf8` | no | Security name |
| `cusip` | `Utf8` | no | 9-char CUSIP (primary join key) |
| `lei` | `Utf8` | yes | 20-char LEI (ISO 17442) |
| `shares` | `Float64` | no | Balance, can be fractional |
| `market_value_usd` | `Float64` | no | Fair value in USD |
| `weight` | `Float64` | no | Fraction of NAV, 0.0 - 1.0 |
| `issuer_cik` | `Utf8` | yes | Always null in v1.0 |

Compression: ZSTD level 3. Row group size: 10 000 rows. Typical file size
is 30-80 KB per month per index.

### N-PORT XML extraction

N-PORT `primary_doc.xml` contains:

- `<headerData>/<filerInfo>/<seriesClassInfo>` -- series/class ID (multi-
  series trusts).
- `<formData>/<genInfo>` -- series ID (preferred source), series name,
  reporting period end.
- `<formData>/<invstOrSecs>/<invstOrSec>` -- one element per holding.

indexkit's parser is a single-pass streaming reader (`quick-xml`) that:

1. Populates `NportHeader` with series ID, name, and `repPdDate`.
2. Collects each `<invstOrSec>` into `RawHolding`.
3. At the end, filters holdings to `assetCat == "EC"` (common stock) and
   maps them into `Constituent` rows sorted by descending weight.

Non-equity holdings (cash, repos, futures) are dropped. This is the
behaviour every index-replicator consumer wants -- "what were the 500
stocks this month" -- and matches the public meaning of the index.

## Refresh schedule

| Workflow | Trigger | Action |
|---|---|---|
| `nightly.yml` | `0 7 * * 1-5` (07:00 UTC, Mon-Fri) | `nightly-append` -- look for any new N-PORT filings since last run |
| `backfill.yml` | `workflow_dispatch` (manual) | `backfill` -- full historical fetch (Nov 2019 onwards) |
| `release.yml` | `v*` tag push | CI gate + `cargo publish` of both crates |

Note on lag: ETFs have up to 60 days to file N-PORT after each reporting
month end. SEC then delays public release by another 30 days. So the
"latest" month in the repo is typically 2-3 months old. This is unavoidable
at the N-PORT level; nothing indexkit can do about it.

## Series filtering

Multi-series trusts (iShares Trust CIK 0001100663 hosts IVV, IJH, IJR,
plus ~100 other ETFs) file one NPORT-P per series per month. indexkit must
open each XML to read the `<seriesId>` before deciding whether to keep it.

Single-series trusts (DIA CIK 0001041130, QQQ CIK 0001067839) have one
NPORT-P per month that is always the target fund; no filtering needed.

The `SecClient::filter_to_series` method inserts a 120 ms sleep between
requests to stay comfortably under the SEC's 10 req/s rate limit. For a
full iShares Trust backfill (77 months x ~130 ETFs in the trust = ~10 000
filings) this means ~20 minutes per index, so backfills are structured to
list first, then filter in a tight loop.
