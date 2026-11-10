# Changelog

All notable changes to Mac Netinstall are documented here. The format is
based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
The release workflow takes a version's notes from its section below.

## [Unreleased]

## [0.1.2] - 2026-11-10

This release adds RouterOS 6, a version picker and installs from local
`.npk` files, and refuses versions below a device's minimum version.
Mac Netinstall is provided "as is", without any warranty (GPL-3.0 sections
15–16): Netinstall erases the device, use it at your own risk (see the
README's Disclaimer).

**Upgrading from 0.1.1:** your downloaded files are kept: an existing
settings file keeps them after quitting. Turn off "Keep downloaded files
after quitting" in Settings if you want them deleted on quit (new installs
start with it off).

### Added

- Version picker in the profile editor: "Always the newest" channels with
  the version each names now (including RouterOS v6 long-term), RouterOS 7
  releases and the 6.x archive from MikroTik's changelog (with channel tags
  and dates), search, the list's age with a refresh button, and a custom
  version checked against download.mikrotik.com. The list is kept for a day
  (`versions.json`); offline the last list or a built-in one is used.
- RouterOS 6 (6.30 to 6.49.x): v6 file names (`routeros-<arch>-<version>.npk`,
  `powerpc` for ppc), the `v6-long-term` channel (`NEWEST6.long-term`),
  bundle-aware extra packages, checksums from 6.49.13 on. v6 installs boot
  the current RouterOS 7 netinstall kernels. A main package missing for the
  device's architecture (old releases) is a clear "not published" error.
- Install from local `.npk` files: a profile can use picked package files
  instead of downloads. Each must be built for the device's architecture
  (checked before formatting), one must be the main (system) package; the
  version is read from its header.
- Downloaded files are deleted when the app quits (and leftovers at the next
  launch), unless "Keep downloaded files after quitting" is on in Settings.
  Your own scripts and packages are always kept.
- Disclaimer: accepted once in the app before the first start (asked again
  if its text changes); `ni-cli flash` needs `--accept-disclaimer` or
  `MAC_NETINSTALL_ACCEPT_DISCLAIMER=1`. Settings has an About section with
  the version, license, disclaimer and links. The README and the user
  guides have a Disclaimer section.
- The device table shows where the packages came from (downloaded or local
  file) under the version.
- The log line of a discovered device and the model's tooltip show the
  device's minimum RouterOS version.
- Package architecture check before formatting: every package of an install
  is checked against the architecture the device reports in its Netinstall
  hello (the RouterOS package names the architecture it is built for inside
  the file). A package built for another architecture fails the device with
  a clear message before anything is offered to it, so the device is never
  formatted for packages it cannot run.
- Automatic architecture resolution: when a device reports an architecture
  name the app does not know (or its usual choice does not fit), the app
  looks at the `routeros` package of each candidate architecture (those of
  the boot kernel the device asked for) and installs the one built for it,
  logging the choice. If none fits, the device fails with the details needed
  to report the model on GitHub Issues.
- The application log records every field of a device's Netinstall hello, so
  an unknown model can be diagnosed from the log.

### Fixed

- Devices that report plain `mips` in their Netinstall hello (seen on a
  CRS125-24G-1S-2HnD, big-endian MIPS) failed with "unknown architecture";
  `mips` now maps to the `mipsbe` packages.

### Changed

- A version older than the device's minimum version (the `min_os` of its
  Netinstall hello) is now refused before anything is erased
  (`version_below_minimum`), because it can leave the device unusable; a
  profile can still allow it ("Allow below the device's minimum version
  (risky)"), then it is a warning as before.
- The mode script (`mode.scr`) is refused for versions before 7.22,
  including RouterOS 6.
- If the version to install or the device's minimum version cannot be read
  (e.g. a local package of an unknown version), the install is refused
  before formatting (`version_unknown`) unless the profile allows going
  below the minimum.
- Local packages must all be the same version (`package_version_mismatch`),
  and a local extra already inside a local v6 bundle is refused.
- Deleting downloads on quit skips versions another process (`ni-cli`, a
  second app) is using; the setting also covers files `ni-cli` downloaded.
- A picked `.npk` is checked before it is registered; another file of the
  same name is refused instead of replacing it.
- Versions with leading zeros (`06.49`) are invalid.
- The application log shows a device's license key only as its length.
- Settings file version 7 (`keep_downloads`, `disclaimer_accepted_version`).
- Profile editor: the architecture next to the extra-package list now says it
  only selects which list is shown; packages are installed for the device's
  own architecture.

### Hardware validation

- MikroTik CRS125-24G-1S-2HnD (`mipsbe`, hello architecture `mips`),
  RouterOS 7.25.1: installed and rebooted with the fixed build. ✓

## [0.1.1] - 2026-11-09

The first release validated on real hardware: a MikroTik hEX RB750Gr3 (mmips)
was installed from a Mac with all four configuration modes, and a hAP ac²
(arm) with an extra package. On first launch the
app asks for the **Local Network** and the firewall (incoming connections)
permissions: allow both. The build is ad-hoc signed, not notarized: open it
the first time via System Settings → Privacy & Security → **Open Anyway**.

### Added

- macOS permissions sheet at first launch (and from Settings): asks for the
  Local Network and incoming-connection permissions up front, shows what
  macOS answered and opens System Settings at the right pane.
- Log panel: resizable (the height is remembered), newest line first, Pause /
  Resume (new lines are collected and counted meanwhile), identical
  consecutive lines shown once with a count; the export keeps every line,
  oldest first.
- User guide in English and Hungarian
  ([English](https://github.com/SepsiG/Mac-Netinstall/blob/main/docs/user-guide.en.md),
  [Magyar](https://github.com/SepsiG/Mac-Netinstall/blob/main/docs/user-guide.hu.md)),
  linked from the README.

### Changed

- Sends refused by macOS (`EHOSTUNREACH`, Local Network permission missing)
  are one clear `local_network_denied` message, in red in the status line and
  in the device's row, instead of a warning per BOOTP request.
- A refused limited broadcast is retried to the subnet's directed broadcast.
- Documentation status updated for the hardware validation; download names
  in the docs no longer carry a version.

### Fixed

- Frontend build: TypeScript pinned back to 6.0.3 (svelte-check does not
  support TypeScript 7 yet); Dependabot no longer proposes TypeScript majors.

### Hardware validation

MikroTik hEX RB750Gr3 (`mmips`), RouterOS 7.25.1, Mac (Apple Silicon) with a
USB 2.5G Ethernet adapter:

- Default configuration: works (checked in WinBox: the default configuration
  dialog).
- Empty configuration: works.
- Custom script: works.
- Keep existing configuration: works.
- Auto-repeat: works (flashing started immediately when the device appeared).

MikroTik hAP ac² (D53G-5HacD2HnD, `arm`), RouterOS 7.25.1, same Mac: installed
with the `routeros` and `wireless` packages (two packages in one install).

Not yet tested on hardware: the other architectures (arm64, mipsbe, ppc,
tile), the mode script, remove branding, the license key and shutdown.

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

[Unreleased]: https://github.com/SepsiG/Mac-Netinstall/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/SepsiG/Mac-Netinstall/compare/013a829...v0.1.2
[0.1.1]: https://github.com/SepsiG/Mac-Netinstall/compare/33a4fbd...013a829
[0.1.0]: https://github.com/SepsiG/Mac-Netinstall/commit/33a4fbd
