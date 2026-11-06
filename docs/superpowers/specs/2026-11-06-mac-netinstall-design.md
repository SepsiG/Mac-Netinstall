# mac-netinstall — Design Spec

- **Date:** 2026-11-06
- **Status:** Approved (brainstorming), pending implementation plan
- **License:** GPL-3.0

## 1. Goal

A native macOS (Apple Silicon only) application that re-flashes MikroTik RouterBOARD devices via
the Netinstall procedure — functionally equivalent to MikroTik's official Netinstall, which is not
available for macOS. Primary use case: recovering broken/misconfigured devices and bulk
re-installing devices.

### Non-goals (MVP)

- Intel Macs, Windows, Linux builds
- x86 (PXE) and ARM64-UEFI devices
- Privileged BPF helper (only added if hardware validation proves plain UDP insufficient)
- Auto-update of the app
- RouterOS configuration beyond what Netinstall itself supports (packages + config script)
- Shipping any MikroTik binaries inside the app

## 2. Constraints & Decisions

| Topic | Decision |
|---|---|
| Platform | `aarch64-apple-darwin`, macOS 13+ |
| Language | Rust (core), TypeScript + Svelte (UI) |
| GUI shell | Tauri 2 |
| Async runtime | tokio |
| Protocol reference | [pyNetinstall](https://github.com/rfdrake/pynetinstall) (GPL-3.0), based on merlinthemagic's reverse engineering. Porting its logic is allowed since this project is GPL-3.0. |
| Boot kernels | Never bundled. Downloaded at runtime from mikrotik.com (official `netinstall-<ver>.tar.gz`), carved out statically as embedded ELF images (verified feasible, see roadmap "Facts"), cached. Supported: mipsbe, mmips, powerpc (generic/e500/440), arm, arm64, tile. x86 (PXE) and ARM64 UEFI are out of MVP. |
| RouterOS packages (.npk) | Downloaded at runtime from mikrotik.com by version + arch, cached. User may also supply local files. |
| Network access | Plain UDP sockets bound to the selected interface (`IP_BOUND_IF`), no root. Behind a `Transport` trait so a BPF transport can be added later. |
| UI language | English default, Hungarian translation. Auto-detect from macOS locale, switchable in settings. |

## 3. Architecture

Cargo workspace:

```
mac-netinstall/
├── crates/
│   ├── ni-proto/    # Netinstall UDP protocol: message types, encode/decode, per-device state machine. Pure, no I/O.
│   ├── ni-bootp/    # BOOTP/DHCP server: answers Etherboot requests, assigns IP from pool, picks boot file by arch.
│   ├── ni-tftp/     # Minimal read-only TFTP server serving boot kernels.
│   ├── ni-assets/   # Boot kernel extraction from official package; NPK download, verification, cache.
│   ├── ni-net/      # macOS networking: interface enumeration, interface-bound UDP sockets, broadcast. `Transport` trait.
│   ├── ni-engine/   # Orchestrator: per-MAC sessions, concurrency limit, profiles, auto-repeat, event stream.
│   ├── ni-sim/      # Device simulator (router side) for tests.
│   └── ni-cli/      # Thin CLI over ni-engine (debugging, headless use, hardware validation).
└── app/             # Tauri app: Rust commands → ni-engine; Svelte + TS frontend.
```

### Unit responsibilities and interfaces

- **ni-proto** — input: bytes + timer ticks; output: protocol events and bytes to send. Fully testable
  without a network. Exact ports and message format are taken from pyNetinstall in the first
  implementation step.
- **ni-bootp** — given a MAC→IP allocator and an arch→boot-filename resolver, answers BOOTP requests
  on the bound interface.
- **ni-tftp** — serves files from a provided read-only file map (filename → path). RRQ only, no WRQ.
- **ni-assets** — `ensure_boot_kernels(version) -> KernelSet`, `ensure_packages(version, arch, names) -> Vec<PathBuf>`.
  Cache in `~/Library/Caches/<bundle-id>/`. Verifies size/checksum; re-downloads on corruption.
- **ni-net** — `list_interfaces()`, `Transport` trait (`send_broadcast`, `send_to`, `recv`), macOS
  implementation via `IP_BOUND_IF` + `SO_BROADCAST`.
- **ni-engine** — owns servers and sessions. Exposes a command API (`start(interface, profile)`,
  `stop()`, `flash(mac, profile)`, `retry(mac)`, `set_auto_repeat(bool)`) and an event stream:
  `DeviceDiscovered`, `Progress`, `Done`, `Failed { code, params }`, `Log`.
- **ni-sim** — simulates one or many devices (BOOTP → TFTP → Netinstall), configurable arch/model,
  fault injection (packet loss, timeouts, disconnect).
- **ni-cli** — `ni-cli --iface en7 --version 7.16 --packages routeros,wifi-qcom --config no-default [--rsc file] [--auto-repeat]`.

## 4. Data Flow (one device)

```
[Router in Etherboot]                         [mac-netinstall]
 1. BOOTP request (vendor class = arch) ───►  ni-bootp: IP from pool, boot filename by arch
 2. TFTP RRQ boot kernel ─────────────────►  ni-tftp: extracted kernel from cache
 3. Kernel boots, Netinstall hello ───────►  ni-proto: new session (MAC, model, arch)
 4.                                     ◄──  ni-engine: profile assigned (auto-repeat or manual)
 5. Format + NPK upload (chunked, acked)
 6. Config option / script transfer
 7. Reboot command ───────────────────────►  session Done → event to UI
```

### Concepts

- **Profile:** RouterOS version (channel: stable / long-term / testing, or exact version), package
  list (main + extras), config mode (`keep`, `no-default`, `custom-script` with `.rsc` file). Stored as
  JSON in Application Support; reusable.
- **Arch resolution:** from BOOTP vendor class and Netinstall hello. Supported arches: arm, arm64,
  mipsbe, mmips, smips, tile, ppc, x86. ni-assets picks NPKs matching the device's arch for the
  profile's version.
- **Multi-device:** one session per MAC, independent state machines. Configurable IP pool (default
  `192.168.88.0/24`, excluding host IP). Configurable concurrency limit (default 1: one device at a time unless chosen in Settings or with `--max-parallel`) since TFTP and
  uploads share bandwidth.
- **Auto-repeat:** each newly discovered MAC gets the active profile automatically. Finished MACs are
  ignored for a configurable cooldown (default 5 min) to avoid re-flashing after reboot.

### Preflight checks (app validates before Start)

- Selected interface is a real wired Ethernet NIC (built-in, USB, Thunderbolt or dock adapter;
  never Wi-Fi, Thunderbolt IP ports, bridges or virtual interfaces) with a **manual IPv4
  configured in macOS** (read from its network service, so it is known without a cable); the
  server address is that one. If not, the app shows exact steps (the app does not change network
  config; no admin rights needed). Without a link, Start is still allowed; serving waits for it.
- macOS Local Network permission (`NSLocalNetworkUsageDescription` in Info.plist).
- No other process bound to the BOOTP/TFTP/Netinstall ports (e.g. Internet Sharing's `bootpd`);
  detected and reported.
- Boot kernels and NPKs for the profile are present or downloadable.

## 5. GUI

Main window:

```
┌───────────────────────────────────────────────────────────┐
│ Interface: [en7 – USB LAN ▾]  IP: 192.168.88.2 ✓  [Start] │
│ Profile:   [hAP ax2 – 7.16 stable ▾]  [Edit]  ☑ Auto-repeat│
├───────────────────────────────────────────────────────────┤
│ Devices                                                   │
│ MAC               Model      Arch   Status        Progress │
│ 48:A9:8A:..:01    hAP ax2    arm64  Uploading     ████░ 72%│
│ 48:A9:8A:..:02    hEX        mmips  Waiting       [Flash]  │
│ DC:2C:6E:..:03    RB951      mipsbe ✓ Done (2m ago)        │
├───────────────────────────────────────────────────────────┤
│ Log ▾  (filter by device, export .txt)                    │
└───────────────────────────────────────────────────────────┘
```

Views:

- **Main:** interface picker, profile picker, device table (per-row Flash / Retry), log panel.
- **Profile editor:** version, package checkboxes, config mode, `.rsc` file picker.
- **Settings:** language, concurrency limit (default 1), auto-repeat cooldown; "Downloaded RouterOS
  files" (size, location, per-version list with integrity state, per-file / per-version / damaged
  / all deletion, each confirmed; a version in use by the engine cannot be deleted); "Advanced":
  DHCP pool override, download timeout.

### i18n

- `svelte-i18n`; `app/src/locales/en.json` (source of truth) and `app/src/locales/hu.json`.
- Rust never returns localized text to the UI — only error/event codes with parameters
  (e.g. `{ "code": "tftp_timeout", "params": { "mac": "..." } }`). The frontend translates.
  CLI output is English.
- CI test: `hu.json` must contain every key present in `en.json`.

## 6. Error Handling

- Per-step timeouts (BOOTP, TFTP, each protocol phase) with bounded retries, then `Failed(code)`.
  A failing device never affects other sessions.
- Manual Retry per device in UI.
- Interface disappears (USB adapter unplugged): engine stops all sessions and reports a clear error.
- NPK / kernel download: size/checksum verification; corrupted cache entries are re-downloaded.
- Package/arch mismatch is validated before flashing starts.
- Structured logging with `tracing`; written to `~/Library/Logs/<bundle-id>/`, exportable from UI.

## 7. Testing

- **ni-proto:** unit tests, golden byte fixtures, `cargo-fuzz` target for the parser.
- **ni-bootp / ni-tftp:** unit tests + loopback integration tests.
- **ni-sim validation chain:** ni-sim is written from pyNetinstall's (hardware-proven) code and
  validated against the official `netinstall-cli` in a Linux x86 VM (roadmap Plan 5; pyNetinstall
  itself cannot run on macOS unmodified). Our engine is tested against ni-sim — including multiple
  simulated devices in parallel, auto-repeat cooldown, and packet-loss injection.
- **Hardware phase (later, when devices are available):** manual checklist, one device per arch
  available; pcap captures become regression fixtures. This phase also decides whether a BPF
  transport is needed.
- **Frontend:** component tests, i18n key completeness test.

## 8. Build & Distribution

- Target `aarch64-apple-darwin` only, minimum macOS 13.
- `cargo tauri build` → `.app` + `.dmg`, ad-hoc signed with the bundle identifier. Code signing /
  notarization optional (requires Apple Developer ID). Unsigned / ad-hoc builds on macOS 15+ open
  via System Settings → Privacy & Security → **Open Anyway** after the first blocked launch
  (Control-click → Open no longer bypasses Gatekeeper there); on macOS 13–14 Control-click → Open
  also works. `NSLocalNetworkUsageDescription` is localized (`en.lproj`, `hu.lproj`
  `InfoPlist.strings`).
- GitHub Actions on `macos-14` (arm64) runner: `cargo fmt --check`, `clippy -D warnings`, tests,
  frontend tests, build artifact.
- `LICENSE` (GPL-3.0) and `NOTICE` crediting pyNetinstall and merlinthemagic's reverse-engineering
  work; README states that MikroTik binaries are downloaded from mikrotik.com at runtime and are not
  redistributed.

## 9. Risks

| Risk | Mitigation |
|---|---|
| Netinstall needs raw L2 frames that UDP sockets cannot produce/receive | `Transport` trait; BPF transport via privileged helper as fallback, decided in hardware phase |
| Config modes (keep / no-default / empty) are not implemented by pyNetinstall; official tool encodes them in `OFFR` fields / `mode.scr` | Plan 5: capture official `netinstall-cli` ↔ ni-sim in a Linux VM; until then only "custom script" (`autorun.scr`) and default behaviour are offered |
| pyNetinstall is incomplete for some arch/RouterOS versions | Hardware phase with pcaps; compare with official `netinstall-cli` in a Linux VM if needed |
| mikrotik.com download URLs / package layout change | URL scheme isolated in ni-assets; local-file fallback always available |
| No test hardware during development | ni-sim + pyNetinstall validation chain; CLI ready for quick hardware validation |
