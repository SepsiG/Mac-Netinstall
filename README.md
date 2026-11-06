# mac-netinstall

Native macOS (Apple Silicon) Netinstall for MikroTik RouterBOARD devices.

Status: early development. See `docs/superpowers/specs/` for the design.

## Development

Requires Rust via rustup (`rust-toolchain.toml` pins the channel).

    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings

License: GPL-3.0 (see LICENSE and NOTICE).
