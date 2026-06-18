# Deferred — post-MVP roadmap

Items intentionally left out of v1 (a lean, personal dev extension). Move an
item out of this file once it ships; add new ideas as they surface.

## Changelog rendering
Show what changed between the installed constraint and the latest version.
Packagist metadata includes each release's `source` (usually a GitHub repo);
fetch release notes / `CHANGELOG.md` and surface them in the diagnostic message
or on hover, as the npm extension does. Mind GitHub API rate limits — prefer
unauthenticated raw `CHANGELOG.md` fetches where possible.

## Update tracks
Offer separate code actions for major / minor / patch / pre-release bumps
instead of a single "latest stable" action, so users can stay within a major.
(Partially covered: version completion now lists every published version so a
within-major version can be picked manually; this would add it as one-click
code actions / Code Lenses.)

## Configurable settings surface
Wire real settings through `lsp.composer-update-checker-lsp.settings`
(initialization options are already passed through): registry URL, cache TTL,
max concurrency, and changelog formatting. Document them in the README.

## Inlay "checking…" hint
A transient inlay hint while Packagist requests are in flight (gated on the
client enabling inlay hints), mirroring the npm extension.
