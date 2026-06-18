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
- **Completes versions**: with the cursor inside a constraint, trigger
  completion (`editor: show completions`) to pick from every published version,
  newest-first, with the latest stable preselected. The chosen version keeps
  your operator (`^7.11` → `^7.12.1`).
- Shows a **Code Lens** — _⬆ Update to ^12.19.0_ — above each outdated line for
  a one-click bump. Code Lens is **off by default in Zed**; enable it with
  `"code_lens": "on"` in your settings (or run `editor: toggle code lens`).

Only files named `composer.json` are inspected; other JSON files are ignored.

## Architecture

A Rust workspace with two crates, mirroring the npm extension:

| Crate | Role |
|-------|------|
| root (`composer-update-checker`) | Thin `zed_extension_api` wrapper. Registers a JSON language server and launches the LSP binary. |
| `lsp/` (`composer-update-checker-lsp`) | A [`tower-lsp`](https://crates.io/crates/tower-lsp) language server: parses the manifest, queries Packagist (TTL cache + bounded concurrency), emits diagnostics and code actions. |

## Install

Open Zed's **Extensions** panel (`zed: extensions`), search for _Composer
composer.json Update Checker_, and install. The extension downloads the matching
language-server binary for your platform automatically — no toolchain required —
and Zed offers updates as new versions are published.

Supported platforms: macOS (arm64/x64), Linux (x64/arm64), Windows (x64).

## Development

Requires a Rust toolchain (`rustup`/`cargo`). Build the language server and put
it on your `PATH`; the extension prefers a `PATH` binary over the downloaded
release, so this overrides the published binary for local iteration:

```bash
cargo build --release -p composer-update-checker-lsp
ln -sf "$(pwd)/target/release/composer-update-checker-lsp" ~/.local/bin/composer-update-checker-lsp
# (ensure ~/.local/bin is on your PATH; /usr/local/bin works too)
```

Then in Zed: command palette → **zed: install dev extension** → select this
repository's root directory. Open any `composer.json` with outdated dependencies
to verify the diagnostics, hover, completion, and the _Update …_ code action.

## Releasing

The extension pins its binary download to `v{version}`, so the release **must
exist before** that version is installed. For each release:

1. Bump `version` in `extension.toml`, `Cargo.toml`, and `lsp/Cargo.toml`.
2. Commit, then push a matching tag: `git tag vX.Y.Z && git push origin vX.Y.Z`.
   The `release` workflow cross-builds the language server for every platform and
   attaches the archives to the GitHub Release.
3. Update the entry in [`zed-industries/extensions`](https://github.com/zed-industries/extensions)
   (submodule pointer + `version` in `extensions.toml`) and open a PR. Merging
   publishes the update to all users.

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
