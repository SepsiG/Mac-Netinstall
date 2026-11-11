# Mac Netinstall — working notes

Native macOS (Apple Silicon) clone of MikroTik Netinstall: Rust workspace +
Tauri 2 / Svelte 5 app, GPL-3.0, English UI with Hungarian translation.

## Where things are documented

- `README.md` — features, development commands, building, releasing.
- `CHANGELOG.md` — what changed in each release.
- `docs/superpowers/specs/2026-11-06-mac-netinstall-design.md` — the design.
- `docs/superpowers/plans/2026-11-06-00-roadmap.md` — facts established per
  plan, hardware results, open checks, and **current status and decisions**
  (last section). Read this first when resuming.
- `docs/superpowers/plans/2026-11-06-0*.md` — the implementation plans.
- `docs/superpowers/research/` — official netinstall-cli wire spec, RouterOS
  v6 downloads and version list.
- `docs/user-guide.{en,hu}.md` — end-user guide.

## Layout

- `crates/ni-proto` sans-IO Netinstall protocol · `ni-sim` device simulator ·
  `ni-net` interfaces (IOKit MACs) · `ni-bootp` · `ni-tftp` · `ni-assets`
  (boot kernels, NPK parsing, downloads) · `ni-engine` (sessions, preflight,
  profiles) · `ni-cli` · `ni-update` (GitHub release check).
- `app/` — Svelte frontend; `app/src-tauri/` — Tauri backend.
  i18n strings in `app/src/locales/{en,hu}.json`; after changing error codes
  run `UPDATE_CODES=1 cargo test -p mac-netinstall --test codes`.

## Conventions

- Toolchain: rustup (`~/.cargo/bin` first in `PATH`; an old MacPorts Rust
  may shadow it). Frontend: `npm ci` only, never `npm install`.
- Before a PR: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `cargo test --workspace`, and in `app/`
  `npm run check && npx vitest run`.
- Work on a branch, open a PR, wait for CI. **The maintainer merges PRs.**
- Releases: see README "Releasing". Only the newest release stays
  published; delete the previous release and tag afterwards.
- Dependabot: TypeScript majors are ignored (svelte-check needs TS ≤ 6);
  check CI before merging other major bumps.
- No Developer ID signing / notarization for now (ad-hoc builds).
