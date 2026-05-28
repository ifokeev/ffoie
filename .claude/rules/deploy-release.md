---
paths:
  - ".github/workflows/**"
  - "packaging/**"
  - "**/Dockerfile*"
  - "compose.yml"
  - "Makefile"
  - "web-client/**"
  - "website/**"
---

# Deploy & release

Reference for the GitHub Pages site and the macOS release pipeline. Loads only
when you touch CI workflows, packaging, Docker, compose, or the web/landing dirs.

## Pages route layout

- `ffoie.net/`         → `website/index.html` (landing)
- `ffoie.net/web/`     → trunk build of `web-client/index.html` (the game)

The workflow builds the game with `--public-url "/web/" --dist dist/web
web-client/index.html`, then `cp -R website/. dist/` lays the landing
files on top. Local trunk builds (`trunk serve web-client/index.html`)
still work for iterating on just the game.

The wasm bundle's chat-server URL is baked at build time via
`FFOIE_CHAT_URL` — see `.claude/rules/engine-internals.md` ("Chat URL").

## macOS release pipeline

`release-macos.yml` fires on `git push --tags` for tags matching `v*`
(or on manual workflow_dispatch). For each run it:

1. Builds `aarch64-apple-darwin` + `x86_64-apple-darwin` and `lipo`s them
   into a universal binary at `target/universal-apple-darwin/release/ffoie`.
2. Assembles `FFOIE.app` from `packaging/macos/Info.plist.template` +
   the binary. `AppIcon.icns` is optional — drop one into
   `packaging/macos/` and it's auto-included.
3. Code-signs with the Developer ID Application identity in a temp
   keychain (cert loaded from secrets), hardened runtime + timestamp.
4. Notarizes via `xcrun notarytool submit --wait`, then `xcrun stapler
   staple`s the ticket onto the .app.
5. Wraps the .app in a DMG (`hdiutil create`), signs the DMG, notarizes
   and staples the DMG.
6. Uploads the DMG as a workflow artifact, and (only when triggered by
   a tag) attaches it to a GitHub Release named after the tag.

Required GitHub secrets: `MACOS_CERTIFICATE` (base64 of .p12),
`MACOS_CERTIFICATE_PASSWORD`, `MACOS_KEYCHAIN_PASSWORD` (any random
string), `MACOS_NOTARIZATION_APPLE_ID`, `MACOS_NOTARIZATION_PASSWORD`
(an app-specific password), `MACOS_TEAM_ID`.

Build-artifact paths:

- Native `target/` is redirected outside iCloud to `/Users/a/.cargo-target/foie/`
  via `../.cargo/config.toml` so iCloud doesn't try to sync GBs of objects.
- Web `dist/` (trunk output) is git-ignored.
