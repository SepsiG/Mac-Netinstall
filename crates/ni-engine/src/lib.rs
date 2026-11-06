//! Orchestration for mac-netinstall (being built; see Plan 4a).

pub mod config;
pub mod event;
pub mod profile;

pub use config::{
    default_pool, interface_usable, resolve_network, subnet, EngineConfig, NetPlan, Ports,
};
pub use event::{DeviceInfoView, DeviceState, EngineEvent, Failure, StartInfo, WaitReason};
pub use profile::{default_profiles_root, ConfigMode, Listing, Profile, ProfileStore};
