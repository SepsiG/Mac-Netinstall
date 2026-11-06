# mac-netinstall

Native macOS (Apple Silicon) Netinstall for MikroTik RouterBOARD devices.

Status: early development. See `docs/superpowers/specs/` for the design.

## Installing the app

The app is ad-hoc signed, not notarized. On macOS 15 and later, the first
launch is blocked: open **System Settings → Privacy & Security**, scroll to
the message about "Mac Netinstall" and click **Open Anyway** (then confirm).
On macOS 13–14, Control-click the app → **Open** works as well.

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

Requires Rust via rustup (`rust-toolchain.toml` pins the channel).

    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings

License: GPL-3.0 (see LICENSE and NOTICE).
