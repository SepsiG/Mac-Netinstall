# Changelog

All notable changes to Mac Netinstall are documented here. The format is
based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
The release workflow takes a version's notes from its section below.

## [Unreleased]

First successful hardware install: hEX RB750Gr3 (`mmips`, RouterOS 7.25.1).

### Added

- User guide in English and Hungarian
  ([docs/user-guide.en.md](docs/user-guide.en.md),
  [docs/user-guide.hu.md](docs/user-guide.hu.md)), linked from the README.
- Log panel: resizable (the height is remembered), newest line first, Pause /
  Resume (new lines are collected and counted meanwhile), identical
  consecutive lines shown once with a count; the export keeps every line,
  oldest first.
- macOS permissions sheet at first launch (and from Settings): asks for the
  Local Network and incoming-connection permissions up front, shows what
  macOS answered and opens System Settings at the right pane.

### Changed

- Sends refused by macOS (`EHOSTUNREACH`, Local Network permission missing)
  are one clear `local_network_denied` message, in red in the status line and
  in the device's row, instead of a warning per BOOTP request.
- A refused limited broadcast is retried to the subnet's directed broadcast.

## [0.1.0] - 2026-11-06

First release: a native macOS (Apple Silicon, macOS 13 or later)
re-implementation of MikroTik Netinstall, as an app and a command line tool.
Not affiliated with MikroTik.

### Added

- **Netinstall protocol** (`ni-proto`): a sans-IO server session aligned with
  the official `netinstall-cli` wire behaviour (captured in a VM and pinned
  by golden fixtures), and a device simulator (`ni-sim`) for end-to-end tests.
- **Network servers** (`ni-net`, `ni-bootp`, `ni-tftp`): interface-bound UDP
  (no root needed), a BOOTP/DHCP responder that hands each architecture its
  boot kernel, and a TFTP server with `blksize`/`tsize`. Only wired Ethernet
  adapters (built-in, USB, Thunderbolt, docks) are offered; the server
  address is the adapter's manual IPv4 from macOS.
- **Assets** (`ni-assets`): RouterOS version resolution per channel, boot
  kernels extracted from the official `netinstall` archive, NPK download with
  SHA-256 and header checks, an offline cache with inventory and selective
  deletion.
- **Engine and CLI** (`ni-engine`, `ni-cli`): several devices at once
  (one at a time by default), profiles (version, architecture packages,
  extra packages, keep / default / empty / script configuration, branding,
  software license key), auto-repeat with a cooldown, "only this MAC",
  cancel/retry/forget, preflight checks with exact macOS advice.
- **App** (Tauri 2 + Svelte 5, English and Hungarian): interface picker
  with preflight advice, profile editor, device table with per-device
  actions, kernel status, log panel and export, settings, a quit
  confirmation while devices are being flashed, window and dialog sizes
  remembered.
- **Packaging**: an app icon, a Dock badge with the number of devices
  being prepared or flashed (and one bounce when one finishes in the
  background), a drag-to-Applications `.dmg`, GitHub Actions CI and a
  release workflow.

### Known limitations

- **Not yet validated on real RouterBOARD hardware**: the protocol is
  aligned with the official tool and tested against the simulator only.
  See the open hardware checks in
  `docs/superpowers/plans/2026-11-06-00-roadmap.md`.
- **Ad-hoc signed, not notarized**: on macOS 15 and later the first launch
  is blocked; allow it in System Settings → Privacy & Security → Open
  Anyway (macOS 13–14: Control-click → Open).
- macOS asks for **Local Network** access on the first use; without it the
  app cannot reach devices.
- Apple Silicon only; x86 (`X86_64__boot`, PXE) and ARM64 UEFI devices are
  not supported.

[Unreleased]: https://github.com/SepsiG/Mac-Netinstall/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/SepsiG/Mac-Netinstall/releases/tag/v0.1.0
