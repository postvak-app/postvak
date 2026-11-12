/*! Postvak.app's own login item on macOS 13 and later (`SMAppService`), which System Settings › General › Login Items
names after the app and lets the user turn off. It registers the agent the app carries in
`Contents/Library/LaunchAgents/` (`packaging/macos/login-item.plist`). */
// objc2 marks every SMAppService call unsafe because the class exists only from macOS 13; `available` checks that.
#![allow(unsafe_code)]

use anyhow::{Result, anyhow};
use objc2::rc::Retained;
use objc2_foundation::NSString;
use objc2_service_management::{SMAppService, SMAppServiceStatus};

use super::LOGIN_ITEM_LABEL;

/// Whether this macOS has `SMAppService`.
pub(super) fn available() -> bool {
    objc2::available!(macos = 13.0)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum State {
    Off,
    On,
    /// Registered, but the user turned it off in System Settings or has yet to allow it there.
    NeedsApproval,
}

pub(super) fn state() -> State {
    // SAFETY: callers check `available` first, so the class exists.
    match unsafe { service().status() } {
        SMAppServiceStatus::Enabled => State::On,
        SMAppServiceStatus::RequiresApproval => State::NeedsApproval,
        _ => State::Off,
    }
}

pub(super) fn register() -> Result<()> {
    // SAFETY: as in `state`.
    unsafe { service().registerAndReturnError() }.map_err(|e| {
        anyhow!(
            "macOS did not add the login item: {}",
            e.localizedDescription()
        )
    })
}

pub(super) fn unregister() -> Result<()> {
    if state() == State::Off {
        return Ok(());
    }
    // SAFETY: as in `state`.
    unsafe { service().unregisterAndReturnError() }.map_err(|e| {
        anyhow!(
            "macOS did not remove the login item: {}",
            e.localizedDescription()
        )
    })
}

/// Opens System Settings › General › Login Items, where the user allows the login item.
pub(super) fn open_settings() {
    // SAFETY: as in `state`.
    unsafe { SMAppService::openSystemSettingsLoginItems() }
}

fn service() -> Retained<SMAppService> {
    let plist = NSString::from_str(&format!("{LOGIN_ITEM_LABEL}.plist"));
    // SAFETY: as in `state`; the name is a plain file name in the app's LaunchAgents folder.
    unsafe { SMAppService::agentServiceWithPlistName(&plist) }
}
