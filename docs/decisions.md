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

