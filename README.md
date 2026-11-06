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
adapter's manual IPv4 from macOS (advanced).

## Development

Requires Rust via rustup (`rust-toolchain.toml` pins the channel).

    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings

License: GPL-3.0 (see LICENSE and NOTICE).
