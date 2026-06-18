# Composer composer.json Update Checker (Zed)

Highlights outdated [Composer](https://getcomposer.org) packages directly in
`composer.json` and offers one-click version bumps — a Composer counterpart to
[`zed-npm-update-checker`](https://github.com/e-simpson/zed-npm-update-checker).

Latest versions come from the [Packagist v2 metadata API](https://packagist.org/apidoc)
(`https://repo.packagist.org/p2/{vendor}/{package}.json`).

## What it does

- Scans `require` and `require-dev` (platform requirements like `php` and
  `ext-*` are skipped).
- Publishes a **diagnostic** on each outdated package: `Update available: ^10.0 → 12.19.0`.
  The diagnostic links to the package's [Packagist](https://packagist.org) page.
- Shows a **hover** on any dependency with its update status plus links to its
  **Packagist** page and **source repository** — the repo link appears whenever
  the package publishes one in its Packagist metadata. Up-to-date packages get a
  hover too, confirming the latest version.
- Offers a **code action** — _Update vendor/package to ^12.19.0_ — that rewrites
  the constraint, preserving your operator (`^`, `~`, or an exact pin).

Only files named `composer.json` are inspected; other JSON files are ignored.

## Architecture

A Rust workspace with two crates, mirroring the npm extension:

| Crate | Role |
|-------|------|
| root (`composer-update-checker`) | Thin `zed_extension_api` wrapper. Registers a JSON language server and launches the LSP binary. |
| `lsp/` (`composer-update-checker-lsp`) | A [`tower-lsp`](https://crates.io/crates/tower-lsp) language server: parses the manifest, queries Packagist (TTL cache + bounded concurrency), emits diagnostics and code actions. |

## Build & install (dev extension)

Requires a Rust toolchain (`rustup`/`cargo`).

```bash
# 1. Build the language server
cargo build --release -p composer-update-checker-lsp

# 2. Put it on your PATH (the extension resolves it via `which`)
ln -sf "$(pwd)/target/release/composer-update-checker-lsp" ~/.local/bin/composer-update-checker-lsp
# (ensure ~/.local/bin is on your PATH; /usr/local/bin works too)
```

Then in Zed: open the command palette → **zed: install dev extension** → select
this repository's root directory.

Open any `composer.json` with outdated dependencies to verify the diagnostics
and the _Update …_ code action.

## Tests

```bash
cargo test -p composer-update-checker-lsp
```

Unit tests cover constraint comparison, manifest parsing/ranges, latest-stable
selection, and repository-URL normalization (SSH→`https`, `.git` stripping,
inheritance across minified metadata entries) — no live network.

## Acknowledgements

Ported from [`zed-npm-update-checker`](https://github.com/e-simpson/zed-npm-update-checker)
by Evan Simpson, an MIT-licensed Zed extension for npm. This Composer
counterpart mirrors its architecture and behavior.

## License

[MIT](LICENSE). This extension incorporates work from the MIT-licensed
`zed-npm-update-checker`; both copyright notices are retained in `LICENSE`.
