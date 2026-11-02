# Decisions log

Deviations from the original spec, and why.

## 2026-11-02 — High ports instead of :80/:443 (Phase 0 spike)

Stock macOS denies non-root processes binding privileged ports (`EACCES`;
there is no `kern.lowest_unprivileged_port` sysctl). The user chose
**"high ports only — no sudo ever"** over a one-time-sudo pf redirect or
launchd socket activation.

Consequences:
- Defaults: `http_port = 8080`, `https_port = 8443`, `mysql_port = 3307`
  (all in `~/Library/Application Support/ferro/config.toml`).
- Site URLs are `https://<name>.localhost:8443` everywhere the spec says
  `https://<name>.localhost`. WP-CLI `core install` URLs, search-replace
  targets, and the menu bar app must all include the port.
- `ferro doctor` port checks target the configured ports, not :80/:443.

## 2026-11-02 — PHP binaries: static-php-cli "bulk" flavor (Phase 0 spike)

The `common` flavor of dl.static-php.dev builds lacks `mysqli`, `intl`,
`sodium`, and OPcache; WordPress requires `mysqli`. Ferro bundles the
`bulk` flavor (verified working end-to-end with php-fpm + WordPress; see
docs/spike.md). ~90 MB per binary (arm64).

## 2026-11-02 — MySQL default TCP port 3307

The dev machine already runs Homebrew mysqld on :3306. The Unix socket is
the primary connection path; TCP defaults to 3307 and is configurable.

## 2026-11-02 — MySQL site users get localhost + 127.0.0.1 + ::1 grants

With `skip-name-resolve`, MySQL's `'user'@'localhost'` matches only Unix
socket connections; TCP from loopback needs `'user'@'127.0.0.1'` (and
`'user'@'::1'`). Adminer/TablePlus connect over TCP. Found because Adminer
auto-login failed with "Host '127.0.0.1' is not allowed to connect".

## 2026-11-02 — php-fpm pools restart, not reload

SIGUSR2 reload works for adding pools, but combined with runtime-dir
deletion on `ferro destroy` it can leave a pool bound to an unlinked
socket (502s until restart). fpm masters now restart (sub-second) when
their generated config actually changes; regeneration compares content
and is idempotent.

## 2026-11-02 — Adminer: mysql-en single-file build, verified differently

Adminer publishes no checksums. We pin the version (6.2.0), verify with
`php -l`, a size floor, and an "Adminer" content marker. Auto-login uses
the documented Adminer\Plugin API (credentials/database/login); the site
picker is a cookie-based bootstrap in front of adminer.php so no password
ever appears in a URL.


## 2026-11-02 — ferro replace: shadow database swap

The streaming pipeline `mysqldump | replace | mysql` cannot import into
the database it is dumping: the dump's `--single-transaction` consistent
read holds metadata locks, so the import's `DROP TABLE` waits on the
dump, which is back-pressured by the stalled import — a deadlock on any
table bigger than the pipe buffers. `ferro replace` (rust engine) now
streams into `<db>__replace_new` and finishes with a single atomic
`RENAME TABLE` swap (old tables moved to `<db>__replace_old`, then
dropped). Side effect: the original data survives a failed replace.
Runs as root over the socket because the per-site MySQL user only has
grants on its own database.

## 2026-11-02 — mysqldump escapes double quotes (and we must too)

Verified against real dumps: mysqldump escapes `"` as `\"` everywhere
inside string literals, including the structural quotes of serialized
data (`s:33:\"...\"`) and quotes inside JSON content. ferro-replace's
SQL mode therefore parses both `:"` and `:\"` header forms, unescapes
content to logical bytes, recurses in raw mode, and re-escapes on
output (structural quotes written as `\"`). Earlier versions matched
only the raw form and replaced inside serialized strings without
recomputing lengths, corrupting the data.

## 2026-11-02 — replace parity vs wp-cli: one intentional superset

On the fixture DB the rust engine is byte-identical to
`wp search-replace` except one row: a plain (non-serialized) option
holding a JSON-escaped URL (`https:\/\/...`). wp-cli only matches the
literal needle there; ferro-replace also replaces the JSON-escaped
variant, which the spec requires for pull correctness. Transient/cron
rows and AUTO_INCREMENT values differ run-to-run (WordPress writes
timestamps on boot) and are excluded from the comparison.

## 2026-11-02 — media proxy fetches via /usr/bin/curl

Proxy mode shells out to curl (always present on macOS) instead of
adding an HTTP client stack: TLS, redirects, HTTP/2 and system proxy
settings come for free, and the daemon stays small. Headers are parsed
off `curl -D -` (redirect blocks skipped), the body streams through a
tokio duplex so responses are never buffered in memory, and
--cache-media writes to a `.ferro-part` sibling renamed on completion.
stderr is piped and logged on failure — Stdio::null() made an early
failure undebuggable.

## 2026-11-02 — pull: SSH via the user's ssh, tested via FERRO_SSH_BIN

`ferro pull` shells out to ssh (BatchMode) so ~/.ssh/config, agents and
host aliases apply, and to rsync for files. No remote server was
available for acceptance, so the end-to-end test substitutes
FERRO_SSH_BIN with a stub that execs the "remote" commands locally
(rsync is routed through it via RSYNC_RSH); the "remote" was a normal
ferro site. A full pull (WP 7.x, rsync, streamed 100 MB DB import with
URL replacement, wp-config rewrite, media proxy) completed in ~7.7 s.
The ≥1 GB real-remote acceptance remains to be re-run against a real
server; the streaming DB path is the same code that benchmarked at
~12 MB/s import-bound in Phase 4.

Other pull decisions: the progress flag is only passed to GNU rsync
(macOS openrsync rejects --info=progress2); wp-config gets
WP_HOME/WP_SITEURL constants pinned to the local URL so hardcoded remote
values cannot win; caching drop-ins (advanced-cache.php, object-cache.php,
db.php) are renamed to .ferro-off; plugin deactivation intersects the
default hostile list with `wp plugin list` (there is no
--skip-if-missing flag); the remote table_prefix is preserved.
