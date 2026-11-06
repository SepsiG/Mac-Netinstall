//! Per-device bookkeeping inside the engine loop.

use super::DeviceView;
use crate::event::{DeviceInfoView, DeviceState};
use crate::profile::Profile;
use ni_bootp::BootArch;
use ni_proto::{DeviceInfo, Mac, ServerSession};
use std::net::Ipv4Addr;
use std::time::Instant;

pub(crate) struct Device {
    pub mac: Mac,
    pub ip: Option<Ipv4Addr>,
    pub boot_arch: Option<BootArch>,
    pub info: Option<DeviceInfo>,
    /// Assigned with `flash`; wins over the active profile.
    pub assigned: Option<Profile>,
    /// The profile of the current or queued attempt.
    pub using: Option<Profile>,
    /// `None` until the first state is set (nothing reported yet).
    pub state: Option<DeviceState>,
    /// Position in the concurrency queue while waiting for a slot.
    pub queued: Option<u64>,
    /// Incremented per preparation, so stale results are dropped.
    pub attempt: u64,
    pub session: Option<ServerSession>,
    /// When the last throttled progress state was reported.
    pub last_progress: Option<Instant>,
}

impl Device {
    pub fn new(mac: Mac) -> Self {
        Device {
            mac,
            ip: None,
            boot_arch: None,
            info: None,
            assigned: None,
            using: None,
            state: None,
            queued: None,
            attempt: 0,
            session: None,
            last_progress: None,
        }
    }

    pub fn is_active(&self) -> bool {
        self.state.as_ref().is_some_and(DeviceState::is_active)
    }

    pub fn is(&self, name: &str) -> bool {
        self.state.as_ref().is_some_and(|s| s.name() == name)
    }

    pub fn view(&self) -> Option<DeviceView> {
        Some(DeviceView {
            mac: self.mac,
            ip: self.ip,
            boot_arch: self.boot_arch.map(|a| a.as_str().to_string()),
            info: self.info.as_ref().map(DeviceInfoView::from),
            profile: self
                .using
                .as_ref()
                .or(self.assigned.as_ref())
                .map(|p| p.name.clone()),
            state: self.state.clone()?,
        })
    }
}
