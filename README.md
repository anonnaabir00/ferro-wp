# Ferro WP

A fast, macOS-only local WordPress environment — think "Laravel Herd for
WordPress": no main window, a menu bar app, a CLI, and a background
daemon.

Drop a WordPress folder into `~/Sites/foo` and it is served at
`https://foo.localhost:8443`. No Docker, no VMs, no `/etc/hosts` edits,
nothing ever runs as root.

## Features

- **Zero-config sites** — parked folders in `~/Sites` (or `ferro link`)
  are served over HTTPS with a local CA and per-site certificates.
- **Fresh sites in seconds** — `ferro new blog` downloads WordPress once
  (cached), creates the database, installs, and prints a working
  `https://blog.localhost:8443` in under ~10 s warm.
- **Bundled everything** — static PHP 8.1–8.4, MySQL 8.4, WP-CLI,
  Composer, Adminer, Mailpit. Nothing to install separately.
- **First-class WP-CLI** — `wp`, `php`, `composer` and `mysql` shims in
  `~/.ferro/bin` pick the right PHP version and DB socket per folder.
- **One-click admin login** — `ferro open <site> --admin` lands you in
  wp-admin already logged in (single-use, loopback-only tokens).
- **Database UI** — Adminer at `https://adminer.localhost:8443`, with a
  site picker and per-site auto-login (`ferro db <site> open`).
- **Fast production pulls** — `ferro pull` streams the remote database
  over SSH (compressed), rewrites URLs on the way through with a
  serialization-aware Rust engine, rsyncs files *without* uploads, and
  serves missing media from production via the media proxy.
- **Search-replace engine** — `ferro replace` is byte-compatible with
  `wp search-replace` and ~1.6× faster on a 1 GB dump (see
  [docs/benchmarks.md](docs/benchmarks.md)).
- **Mail catcher** — Mailpit SMTP on 127.0.0.1:1025, UI at
  `https://mail.localhost:8443`; `wp_mail()` just works on ferro sites.
- **Menu bar app** — native SwiftUI MenuBarExtra with live status,
  per-site actions, and New/Pull sheets. Sites keep running when it
  quits.

## Requirements

- macOS (Apple silicon), no other dependencies — not even Homebrew.
- Ports 8080/8443 (router) and 3307 (MySQL) free. Ferro deliberately
  does not use :80/:443 — macOS refuses non-root binds there, and ferro
  never runs as root. See [docs/decisions.md](docs/decisions.md).

## Install

```sh
git clone <repo> && cd ferro-wp
./scripts/install.sh          # builds ferro, ferrod, shims into ~/.ferro/bin
export PATH="$HOME/.ferro/bin:$PATH"   # add to ~/.zshrc
ferro daemon install && ferro daemon start
ferro doctor                  # verifies everything
ferro trust-ca                # optional: trust the local CA (prompted)
scripts/build-menubar.sh      # optional: menu bar app → ~/Applications/Ferro.app
```

The daemon runs as a launchd agent (`~/Library/LaunchAgents/dev.ferro.daemon.plist`)
and keeps sites alive across reboots and terminal sessions.

## Daily use

```sh
ferro new blog                # fresh WordPress at https://blog.localhost:8443
ferro open blog --admin       # wp-admin, already logged in
cd ~/Sites/blog && wp plugin list   # shims: right PHP + DB automatically

mkdir ~/Sites/mysite && cp -r ~/old-site/* ~/Sites/mysite   # parked: just works

ferro pull prod@example.com /var/www/html --name client
ferro replace client https://old.test https://client.localhost:8443
ferro db client open          # Adminer, already logged in
```

## CLI reference

| command | what it does |
|---|---|
| `ferro status` | daemon, services, ports, conflicts |
| `ferro doctor` | deep checks: port owners, PATH shims, CA trust, disk |
| `ferro park [path]` | park a folder (default ~/Sites) |
| `ferro link <name> [path]` | serve an arbitrary folder |
| `ferro new <name> [--php V] [--wp V] [--title T] [--admin-user U] [--multisite] [--blueprint f.toml] [--empty]` | fresh site in seconds |
| `ferro destroy <name> [--yes]` | drop DB, remove config, folder to Trash |
| `ferro clone <name> <new>` | copy files + DB, rewrite URLs |
| `ferro creds <name>` | admin user + Keychain-stored password |
| `ferro open <name> [--admin]` | open site (wp-admin, auto-login) |
| `ferro start\|stop\|restart [name]` | site or service control |
| `ferro php <name> <version>` | switch a site's PHP (8.1–8.4) |
| `ferro logs <name> [--php\|--mysql\|--router] [-f]` | tail logs |
| `ferro db <name> export\|import\|shell\|open\|info` | database tools |
| `ferro adminer` / `ferro mail` | Adminer picker / Mailpit UI |
| `ferro pull <ssh> <path> [--name N] [--no-media-proxy] [--include-uploads] [--media-mode M] [--cache-media]` | pull production to local |
| `ferro replace <name> <from> <to> [--engine rust\|wp-cli]` | serialization-aware search-replace |
| `ferro wp <name> -- <args>` | WP-CLI from anywhere |
| `ferro update [php\|mysql\|wp-cli\|composer\|adminer\|mailpit\|wordpress-cache]` | refresh bundled components |

All commands support `--json`.

## How it works

```
SwiftUI MenuBarExtra app  ─┐
                           ├── Unix socket (JSON-RPC) ──► ferrod (launchd agent)
`ferro` CLI (Rust) ────────┘                                ├─ router (8080/8443, *.localhost, TLS)
`wp`/`php`/`mysql` shims ──► site lookup ──────────────────┤  supervisor (php-fpm, mysqld, mailpit)
                                                           ├─ site registry (parked + linked)
                                                           └─ login tokens, events
```

- The router terminates TLS (local root CA, per-site SNI certs), serves
  static files, and talks FastCGI to one php-fpm pool per site.
- One shared MySQL 8.4 with a database + user per site, on a Unix
  socket plus 127.0.0.1:3307.
- `*.localhost` resolves to loopback natively on macOS — no DNS daemon,
  no `/etc/hosts`.
- App data: `~/Library/Application Support/ferro/` — logs:
  `~/Library/Logs/ferro/`.

More detail: [docs/spike.md](docs/spike.md),
[docs/decisions.md](docs/decisions.md),
[docs/benchmarks.md](docs/benchmarks.md).

## Known limitations

- macOS only, Apple silicon first.
- Xdebug is not available: the prebuilt static PHP binaries
  (static-php-cli "bulk" flavor) do not include it, and static PHP does
  not load dynamic extensions. A custom static-php-cli build with
  Xdebug compiled in is future work (tracked in docs/decisions.md).
- The menu bar app targets macOS 15+.
- Mailpit's UI works at `mail.localhost` but without websocket live
  updates (new mail shows on refresh).

## Development

```sh
cargo build                          # workspace: ferro-core, ferrod, ferro-cli,
                                     # ferro-router, ferro-replace, ferro-import
cargo test                           # unit + property tests
cargo test --features integration    # + end-to-end (needs bundled binaries)
cargo clippy --all-targets --all-features -- -D warnings
swift build                          # macos/FerroMenuBar
```

Quality bar: clippy-clean, `cargo fmt`, user-facing errors with a
suggested fix, integration tests behind the `integration` feature.
