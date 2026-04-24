# Upstream source LICENSE files

This directory ships verbatim copies of the licenses of each OSS data
source ingested by indexkit's `github-backfill` command. MIT and Apache-
2.0 both require the copyright-notice to be retained when the covered
work is redistributed; shipping the unmodified LICENSE text is the
cleanest way to comply.

| File | Upstream | SPDX | Consumed by |
|---|---|---|---|
| `fja05680-LICENSE` | [fja05680/sp500](https://github.com/fja05680/sp500) | MIT | `DataSource::GithubFja05680` |
| `yfiua-LICENSE` | [yfiua/index-constituents](https://github.com/yfiua/index-constituents) | Apache-2.0 | `DataSource::GithubYfiua` |
| `hanshof-LICENSE` | [hanshof/sp500_constituents](https://github.com/hanshof/sp500_constituents) | MIT | `DataSource::GithubHanshof` |

indexkit itself is Apache-2.0 licensed (see the repository-root `LICENSE`
file). The OSS data sources are NOT relicensed under indexkit's Apache-
2.0; the derivative parquet files continue to carry their upstream
copyright and license semantics where applicable.
