# Phase 0 — Spike findings

Date: 2026-11-02. Machine: arm64 Mac (Apple Silicon), macOS 27.0.1, user `anonnaabir` (non-root), Rust 1.98.1 via Homebrew.

Product name: **Ferro WP** (working name `wpx` renamed). CLI `ferro`, daemon `ferrod`,
app data `~/Library/Application Support/ferro`, shims `~/.ferro/bin`.

## 1. Non-root bind of 127.0.0.1:80/443 — FAILED

Binding ports 80/443 as a normal user is denied by macOS (`EACCES`, errno 13):

```
bind 127.0.0.1:80 FAILED: [Errno 13] Permission denied
bind 127.0.0.1:443 FAILED: [Errno 13] Permission denied
```

- There is no `kern.lowest_unprivileged_port` sysctl on macOS (`sysctl: unknown oid`).
- Laravel Herd (installed on this machine) works around this with a **privileged
  helper**: `/Library/LaunchDaemons/de.beyondco.herd.helper.plist`, installed once
  with sudo. The Homebrew nginx currently holding :80/:443 was started via
  `sudo brew services` (root LaunchDaemon `homebrew.mxcl.nginx`).
- Viable options (decision needed, see below):
  1. **pf redirect + small root LaunchDaemon** (one-time sudo): redirect
     loopback 80→8080 and 443→8443; ferrod itself stays non-root on high ports.
     This is how clean `https://foo.localhost` URLs survive.
  2. **launchd socket activation**: a root LaunchDaemon binds :80/:443 and passes
     the sockets to the user-space daemon. Runtime fully non-root; install needs
     sudo once. Cleaner at runtime, more moving parts.
  3. **High ports only** (`https://foo.localhost:8443`): zero sudo, but breaks
     the clean-URL product goal and complicates multisite/Adminer UX.

## 2. *.localhost resolution — PASSED

macOS resolves any subdomain of `localhost` to loopback with zero configuration
(verified via `dscacheutil` and `ping` for `foo.localhost` and a random
`some-test-12345.localhost`). No /etc/hosts edits, no dnsmasq, no DNS daemon needed.

## 3. static-php-cli PHP 8.3 + php-fpm + WordPress — PASSED (with a caveat)

Prebuilt static binaries from `dl.static-php.dev` (project: static-php-cli):

- The **`common`** build flavor is **NOT sufficient**: it lacks `mysqli`,
  `intl`, `sodium`, `opcache` — WordPress would die with the
  "missing MySQL extension" error.
- The **`bulk`** build flavor has everything WordPress needs:
  `mysqli, mysqlnd, pdo_mysql, curl, gd, imagick, intl, mbstring, zip, xml,
  exif, sodium, apcu, redis, Zend OPcache`, and more.
- Verified with `php-8.3.32-{cli,fpm}-macos-aarch64.tar.gz` (bulk):
  - `php -v` runs (NTS, static, ~90 MB per binary).
  - `php-fpm` starts and listens on 127.0.0.1:9777 as the user.
  - WordPress (latest.zip) executes: `GET /` → 302 → `wp-admin/setup-config.php`.
  - Full chain proven end-to-end: throwaway nginx (high port, custom conf,
    no Homebrew service touched) → FastCGI → php-fpm → WordPress → 302 to setup.

**Decision recorded**: Ferro will bundle the **bulk** flavor of static-php-cli
builds for PHP 8.1–8.4 (arm64). Build provenance/checksum verification will be
part of `scripts/fetch-php.sh` (Phase 2).

## Existing port conflicts on this machine (relevant for Phase 1 doctor)

- `:80`/`:443` — Homebrew nginx (root LaunchDaemon) + Herd helper present.
- `:3306` — Homebrew `sh.brew.mysql` mysqld running as the user.
- `ferro doctor` must detect and name these, and MySQL will default to a
  non-standard port (e.g. 3307) unless the user frees 3306.

## Conclusion

Assumptions 2 and 3 pass. Assumption 1 fails on stock macOS and needs a product
decision before Phase 1 (pf redirect vs launchd socket activation vs high ports).
