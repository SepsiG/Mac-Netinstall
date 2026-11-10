# mac-netinstall

[![CI](https://github.com/SepsiG/Mac-Netinstall/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/SepsiG/Mac-Netinstall/actions/workflows/ci.yml)

Native macOS (Apple Silicon) Netinstall for MikroTik RouterBOARD devices.
Not affiliated with MikroTik.

Status: validated on a MikroTik hEX RB750Gr3 (`mmips`) with all four
configuration modes (default, empty, script, keep) and auto-repeat, and on a
hAP ac² (`arm`) with an extra package, and on a CRS125-24G-1S-2HnD (`mipsbe`);
RouterOS 6.49.23 from the archive was installed on the hEX, and the
minimum-version check works with the values real devices report;
arm64, smips, ppc and tile are not yet tested on hardware (see [CHANGELOG.md](CHANGELOG.md)). See `docs/superpowers/specs/` for the design.

**Disclaimer:** provided “as is”, without any warranty (GNU GPL v3, sections
15–16). Netinstall erases the device: use it at your own risk. The authors
accept no liability for any damage, including devices that become unusable
(“bricked”). See [Disclaimer](#disclaimer).

**User guide:** [English](docs/user-guide.en.md) · [Magyar](docs/user-guide.hu.md)
— installing, preparing the Mac, profiles, flashing, troubleshooting.

## Installing the app

Requires an Apple Silicon Mac with macOS 13 or later.

1. Download `Mac-Netinstall_<version>_aarch64.dmg` and
   `Mac-Netinstall_<version>_aarch64.dmg.sha256` from the
   [Releases](https://github.com/SepsiG/Mac-Netinstall/releases) page.
   Optionally check the download, in the folder that holds both:

       shasum -a 256 -c Mac-Netinstall_<version>_aarch64.dmg.sha256

   It prints `Mac-Netinstall_<version>_aarch64.dmg: OK`.
2. Open the `.dmg` and drag **Mac Netinstall** onto **Applications**.
3. First launch: the app is ad-hoc signed, not notarized. On macOS 15 and
   later the first launch is blocked: open **System Settings → Privacy &
   Security**, scroll to the message about "Mac Netinstall" and click
   **Open Anyway** (then confirm). On macOS 13–14, Control-click the app →
   **Open** works as well.
4. **Local Network:** macOS asks whether Mac Netinstall may find and
   connect to devices on the local network the first time it talks to a
   device; allow it. If it was denied, turn it on in System Settings →
   Privacy & Security → Local Network.

**Updates:** the app checks GitHub for a newer release at most once a day
(one request to `api.github.com`, nothing else is sent; turn it off in
Settings → About). A newer version is announced in a banner; **Download**
saves the DMG to `~/Downloads`, verifies it against the published
`.sha256` and opens it (not while devices are being flashed); replace the
app in Applications and restart it. The DMG is marked as downloaded from
the Internet, so macOS Gatekeeper checks the new app; since it is not
notarized, the first launch may need **Open Anyway** again. The `.sha256`
check protects against corrupted or incomplete downloads, not against a
compromised release.
`ni-cli version --check` does the same comparison from the command line.

While it runs, the app is in the Dock; the Dock icon's badge shows how many
devices are being prepared or flashed.

Before starting, give the Ethernet adapter the router is connected to a
manual IPv4 address in macOS (System Settings → Network → the adapter →
Details → TCP/IP → Configure IPv4: Manually, e.g. 192.168.88.1 /
255.255.255.0). The app lists only wired Ethernet adapters (built-in, USB,
Thunderbolt or dock adapters) and flashes one device at a time unless
Settings say otherwise.

## Command line

`ni-cli interfaces` lists the interfaces a RouterBOARD can be netinstalled
through (`--all`: every interface). `ni-cli flash` flashes one device at a
time; `--max-parallel N` allows more. `--server-ip`/`--netmask` override the
adapter's manual IPv4 from macOS (advanced). `flash`/`preflight
--allow-any-interface` (advanced) accepts an interface that is not a wired
Ethernet adapter, with a warning; the app never offers one.

`ni-cli flash` prints the disclaimer and runs only after it is accepted
with `--accept-disclaimer` (or `MAC_NETINSTALL_ACCEPT_DISCLAIMER=1`).

Profiles (`ni-cli profiles …`, and the app's) are JSON files readable by
their owner only (mode 0600), since they may hold a software license key.
`ni-cli` never prints the key: `profiles list/show/save` (also with
`--json`) show it masked as `****<last 4 characters> (<length> chars)`.

## Release notes

- **One device at a time by default.** `max_parallel` now defaults to 1 (it
  was 4) in the app, the engine and `ni-cli flash`. Settings saved by an
  older app with the old default 4 are changed to 1 the first time the new
  app starts (written back as settings version 2); any other value you chose
  is kept. To flash several devices at once, raise "Devices at once" in
  Settings (or pass `--max-parallel`).

- **Window and dialogs remember their size.** The main window comes back
  where and as large as you left it (on a display that is still connected),
  and every dialog can be resized with the grip in its bottom-right corner;
  its size is kept for next time (settings version 4).

## Development

Requires Rust via rustup (`rust-toolchain.toml` pins the channel) and, for
the app, Node.js 24 or later.

    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings
    cd app && npm ci && npm run check && npx vitest run

A few `ni-net` tests compare against the Mac's own network adapters; where
the `CI` environment variable is set (GitHub Actions) they skip themselves,
unless `NI_HARDWARE_TESTS=1`. Tests that need the internet are `#[ignore]`d
(`cargo test -- --ignored` runs them).

**App icon:** `app/icon/icon.svg` (and `icon-small.svg` for 16–64 px) is the
source. After editing it, run `node scripts/make-icon.mjs` in `app/` and
commit the PNGs in `app/src-tauri/icons/` (a test checks they match the SVG;
the bundler builds the `.icns` from them).

**Building the app:** in `app/`,

    CI=true npx tauri build --target aarch64-apple-darwin --bundles app,dmg

writes `target/aarch64-apple-darwin/release/bundle/macos/Mac Netinstall.app`
and `…/bundle/dmg/Mac Netinstall_<version>_aarch64.dmg` (CI and the release
workflow publish it as `Mac-Netinstall_<version>_aarch64.dmg`, without
spaces). `CI=true` skips the
bundler's Finder AppleScript (it needs Automation permission for Finder),
so the DMG has the app and the Applications link in Finder's default
layout; without it, the window layout from `tauri.conf.json`
(`bundle.macOS.dmg`) is applied. CI builds always use `CI=true`.

## Releasing

The version lives in `Cargo.toml` (`[workspace.package] version`),
`app/src-tauri/tauri.conf.json` and `app/package.json` (+ its lock file); a
test fails if they differ, or if `CHANGELOG.md` has no section for it.

1. Bump the version in those files (`npm version --no-git-tag-version
   <x.y.z>` in `app/` updates `package.json` and the lock file), and run
   `cargo check` so `Cargo.lock` follows.
2. In `CHANGELOG.md`, move the `[Unreleased]` notes to a new
   `## [x.y.z] - YYYY-MM-DD` section and update the links at the bottom.
3. Commit, merge to `main`, then tag and push the tag:
   `git tag -a vx.y.z -m "Mac Netinstall x.y.z" && git push origin vx.y.z`.
4. The Release workflow checks that the tag matches the versions, runs the
   tests, builds `Mac-Netinstall_<version>_aarch64.dmg`, and creates a
   **draft** GitHub Release with the `.dmg`, its `.sha256` and the changelog
   section as notes (a version with `-`, e.g. `0.2.0-rc.1`, is marked as a
   pre-release). Review it and publish.

Signing is ad-hoc; `.github/workflows/release.yml` has commented-out hooks
for Developer ID signing and notarization secrets.

License: GPL-3.0 (see LICENSE and NOTICE).

## Disclaimer

- Mac Netinstall is free software, provided **“as is”, without any
  warranty**, as stated in sections 15–16 of the
  [GNU General Public License v3](LICENSE).
- Netinstall **erases the device's storage** and reinstalls RouterOS.
  **Use it at your own risk.**
- The authors and contributors accept **no liability for any damage or
  loss**, including devices becoming unusable (“bricked”), lost
  configuration or data, downtime, or damage resulting from installing old,
  archived or end-of-life RouterOS versions, versions below the device's
  minimum version, or wrong packages.
- Test on a device you can recover (e.g. with MikroTik's official
  Netinstall) before using it in production.
- Not affiliated with, endorsed or supported by MikroTik. MikroTik,
  RouterOS and RouterBOARD are trademarks of their owner.

Magyarul: a szoftvert „ahogy van” alapon, mindenféle garancia nélkül
biztosítjuk; saját felelősségedre használd. Részletek:
[felhasználói útmutató](docs/user-guide.hu.md#13-felelősség-kizárása).
