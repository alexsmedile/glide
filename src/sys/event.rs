// Copyright The Glide Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

use core_graphics::base::CGError;
use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use livesplit_hotkey::{ConsumePreference, Hook};
pub use livesplit_hotkey::{Hotkey, KeyCode, Modifiers};
use objc2_app_kit::NSEvent;
use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGDisplayHideCursor, CGDisplayShowCursor, CGWarpMouseCursorPosition, kCGNullDirectDisplay,
};
use serde::{Deserialize, Serialize};
use tracing::info_span;

use super::screen::CoordinateConverter;
use crate::actor::reactor::Command;
use crate::actor::wm_controller::{Sender, WmCommand, WmEvent};

pub struct HotkeyManager {
    hook: Hook,
    events_tx: Sender,
}

impl HotkeyManager {
    pub fn new(events_tx: Sender) -> Result<Self, livesplit_hotkey::Error> {
        let hook = Hook::with_consume_preference(ConsumePreference::MustConsume)?;
        Ok(HotkeyManager { hook, events_tx })
    }

    /// Creates a hotkey observer that leaves matching events in the system
    /// event stream. Native macOS shortcuts can therefore act on the same
    /// keypress after Glide has inspected it.
    pub fn new_passthrough(events_tx: Sender) -> Result<Self, livesplit_hotkey::Error> {
        let hook = Hook::with_consume_preference(ConsumePreference::MustNotConsume)?;
        Ok(HotkeyManager { hook, events_tx })
    }

    pub fn register(&self, modifiers: Modifiers, key_code: KeyCode, cmd: Command) {
        self.register_wm(modifiers, key_code, WmCommand::ReactorCommand(cmd))
    }

    pub fn register_wm(&self, modifiers: Modifiers, key_code: KeyCode, cmd: WmCommand) {
        let events_tx = self.events_tx.clone();
        let mut seq = 0;
        self.hook
            .register(Hotkey { modifiers, key_code }, move || {
                seq += 1;
                let span = info_span!("hotkey::press", ?key_code, ?seq);
                events_tx.send((span, WmEvent::Command(cmd.clone()))).unwrap()
            })
            .unwrap();
    }
}

/// Replays the configured native Option+Digit Mission Control shortcut.
pub fn post_native_space_shortcut(desktop: usize) -> anyhow::Result<()> {
    let key_code = match desktop {
        1 => 18,
        2 => 19,
        3 => 20,
        4 => 21,
        5 => 23,
        6 => 22,
        7 => 26,
        8 => 28,
        9 => 25,
        _ => anyhow::bail!("native Desktop shortcut must be between 1 and 9"),
    };
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| anyhow::anyhow!("could not create keyboard event source"))?;
    let down = CGEvent::new_keyboard_event(source.clone(), key_code, true)
        .map_err(|_| anyhow::anyhow!("could not create key-down event"))?;
    down.set_flags(CGEventFlags::CGEventFlagAlternate);
    down.post(CGEventTapLocation::Session);
    let up = CGEvent::new_keyboard_event(source, key_code, false)
        .map_err(|_| anyhow::anyhow!("could not create key-up event"))?;
    up.set_flags(CGEventFlags::CGEventFlagAlternate);
    up.post(CGEventTapLocation::Session);
    Ok(())
}

/// The state of the left mouse button.
#[derive(Serialize, Deserialize, Debug, Copy, Clone, Eq, PartialEq)]
pub enum MouseState {
    Down,
    Up,
}

pub fn get_mouse_state() -> MouseState {
    let left_button = NSEvent::pressedMouseButtons() & 0x1 != 0;
    if left_button {
        MouseState::Down
    } else {
        MouseState::Up
    }
}

pub fn get_mouse_pos(converter: CoordinateConverter) -> Option<CGPoint> {
    let ns_loc = NSEvent::mouseLocation();
    converter.convert_point(ns_loc)
}

pub fn warp_mouse(point: CGPoint) -> Result<(), CGError> {
    cg_result(CGWarpMouseCursorPosition(point).0)
}

/// Hide the mouse. Note that this will have no effect unless
/// [`window_server::allow_hide_mouse`] was called or this application is
/// focused.
pub fn hide_mouse() -> Result<(), CGError> {
    cg_result(CGDisplayHideCursor(kCGNullDirectDisplay).0)
}

pub fn show_mouse() -> Result<(), CGError> {
    cg_result(CGDisplayShowCursor(kCGNullDirectDisplay).0)
}

fn cg_result(err: CGError) -> Result<(), CGError> {
    if err == 0 { Ok(()) } else { Err(err) }
}
