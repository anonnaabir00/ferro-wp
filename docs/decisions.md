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

## 2026-11-05 — Adminer moves per-site (`/__adminer/`), picker removed

Adminer was a built-in site at `adminer.localhost` with a cookie-based
site picker. It is now served by the router at a reserved path on each
site (`https://<site>.localhost:<port>/__adminer/`, LocalWP-style): the
host names the site, so the picker, the `ferro_site` cookie, the
`adminer.localhost` built-in site and `ferro adminer` are all gone.
`/__adminer` redirects to the trailing-slash form so Adminer's relative
links stay under the prefix. The PHP runs through the site's own pool
(pools have no chroot/open_basedir, so the app-data-dir document root is
reachable); only `index.php` and `adminer.css` are served there.


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

## 2026-11-02 — menu bar app: SwiftPM + .app wrapper, macOS 15 target

Only the Command Line Tools are installed (no full Xcode), so the
SwiftUI app is a SwiftPM executable target wrapped into
~/Applications/Ferro.app by scripts/build-menubar.sh (Info.plist with
LSUIElement, ad-hoc codesign). Target is macOS 15, not the spec's 13+:
Window scene modifiers (defaultLaunchBehavior, restorationBehavior)
require 15, and the dev machine is far newer. MenuBarExtra itself is
13+ if the floor ever needs lowering.

The app uses daemon IPC for reads (status, site.list), simple setters
(site.set_running / set_php / set_media_proxy), login tokens and the
events stream, and shells out to the ferro CLI for mutations with no
RPC (new, pull, clone, destroy) so there is one implementation of each
flow. Live updates come from the `events` subscription with a 300 ms
debounce — no polling; app start happens in the MenuBarExtra label's
onAppear because the menu content itself is created lazily on first
click. Verified: builds with swift build, launches, holds the event
subscription (lsof), receives stop/start events; menu interactions
themselves were reviewed but not driven headlessly (needs a logged-in
GUI session).

## 2026-11-02 — Xdebug: not available with static-php builds (deviation)

The spec's optional Xdebug toggle is not implemented. The prebuilt
static-php-cli "bulk" binaries we ship (needed for mysqli) are fully
static: no xdebug compiled in, and static PHP cannot load dynamic
extensions. Options are a custom static-php-cli build with Xdebug
(future work) or a different PHP distribution. The `site.xdebug` field
exists in the registry but stays dormant; documented in README.

## 2026-11-02 — Mailpit as a supervised service + router proxy

Mailpit is fetched from GitHub releases (darwin-arm64 tarball, verified
against the sha256 digest the GitHub API publishes per asset — releases
no longer ship a checksums.txt) and supervised by ferrod like MySQL:
SMTP on 127.0.0.1:1025, UI on 127.0.0.1:8025, reverse-proxied at
mail.localhost through the router's curl streaming (no websocket
proxying — the UI works, live updates need a manual refresh). Sites get
a ferro-mail.php mu-plugin that points wp_mail at the local SMTP only
when the FERRO constant is defined, so production copies are unaffected.
`ferro update` embeds the fetch scripts into the CLI binary so updates
work without the repo checkout.

## 2026-11-02 — signed/notarized installer: future work

The spec's signed, notarized app bundle + installer is not done: no
Apple Developer account is available in this environment. The menu bar
app is ad-hoc codesigned by scripts/build-menubar.sh, which is enough
for same-machine use. A distribution build (Developer ID signing,
notarization, installer package) is documented as future work in
README's development section.
