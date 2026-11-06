//! Orchestration for mac-netinstall.
//!
//! - [`EngineHandle`]: the engine task (BOOTP, TFTP and Netinstall for
//!   many devices at once), driven by commands, reporting [`EngineEvent`]s.
//! - [`Profile`] / [`ProfileStore`]: install recipes as JSON files.
//! - [`preflight`]: what to check before starting.
//! - [`codes`]: every stable code, for translation checks.
//!
//! All errors and events carry stable codes ([`Failure`]); nothing here
//! returns localized text. Logging uses `tracing` macros only; installing a
//! subscriber is up to the binary.

pub mod codes;
pub mod config;
pub mod engine;
pub mod event;
pub mod gate;
pub mod preflight;
pub mod prepare;
pub mod profile;

pub use config::{
    check_cooldown, check_max_parallel, check_pool_reserved, default_pool, interface_usable,
    link_status, resolve_network, subnet, EngineConfig, InterfaceSource, LinkStatus, NetPlan,
    Ports, DEFAULT_NETMASK, MAX_COOLDOWN, MAX_PARALLEL, MIN_COOLDOWN,
};
pub use engine::{DeviceView, EngineHandle, Snapshot, EVENT_CAPACITY};
pub use event::{
    DeviceInfoView, DeviceState, EngineEvent, Failure, KernelState, StartInfo, WaitReason,
};
pub use preflight::{Finding, PreflightReport, Severity};
pub use profile::{default_profiles_root, ConfigMode, Listing, Profile, ProfileStore};
