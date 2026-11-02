# Benchmarks

## ferro-replace vs `wp search-replace`

**Date:** 2026-11-02 · **Machine:** Apple silicon (arm64), macOS, bundled
MySQL 8.4.8, PHP 8.3.32 static build, WP-CLI latest.

### Method

Fixture: the `srfix` site database plus 2,400,000 synthetic posts in
`wp_posts` (~576 MB of `post_content`, ~1.15 GB total dump) mixing
plain URLs, PHP-serialized arrays, nested serialization, and
JSON-escaped URLs — the shapes WordPress stores in practice.

Command under test (full user-facing operation, dump + replace + import
for the rust engine; in-place serialized-aware UPDATEs for WP-CLI):

```
ferro replace srfix https://bench.example.com https://replaced.example.com
ferro replace srfix https://bench.example.com https://replaced.example.com --engine=wp-cli
```

The database was restored to the identical starting state before each
run. Wall time measured with `time`.

### Results

Dump size ~1.15 GB (mysqldump output). Both engines were run against
the identical restored database.

| engine | wall time | notes |
|---|---|---|
| rust (ferro-replace, streaming) | **94.2 s** | dump + replace + import overlapped; atomic table swap at the end |
| wp-cli (`wp search-replace`) | 154.6 s | PHP boots WordPress and processes every table |

**ferro-replace is ~1.6× faster end to end** on the 1.15 GB dump, and
unlike WP-CLI its memory use does not depend on table size (bounded by
the largest single serialized string).

An earlier run against a standalone 1 GB table (fewer indexes than
wp_posts) completed in 47.2 s — import speed, not replacement, is the
bottleneck, as the spec requires.

### Notes

- The rust engine streams `mysqldump | replace | mysql` into a shadow
  database and swaps tables atomically at the end (see
  docs/decisions.md), so its cost is bounded by dump + import speed, not
  by per-row UPDATE latency.
- WP-CLI must boot WordPress, read every row into PHP, unserialize,
  replace, reserialize, and UPDATE row by row.
- Scope differences (intentional, documented in docs/decisions.md):
  `wp search-replace` skips the `guid` column and ignores JSON-escaped
  URLs in plain (non-serialized) columns; ferro-replace replaces both,
  which is what a production→local pull needs.
- `wp search-replace` also skips non-WordPress tables by default;
  ferro-replace covers the whole database.
