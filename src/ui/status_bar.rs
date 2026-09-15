// Copyright The Glide Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Menu bar icon for displaying the current space ID.

use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_core_foundation::CGSize;
use objc2_foundation::{NSData, NSObject, NSString, ns_string};
use tracing::{Span, debug, error, warn};

use crate::actor::reactor;
use crate::actor::wm_controller::{self, WmCmd, WmCommand, WmEvent};
use crate::config;

const SAVE_AND_QUIT_TAG: i64 = 1;
const TOGGLE_GLOBAL_TAG: i64 = 2;
const TOGGLE_SPACE_TAG: i64 = 3;
const SHOW_DOCS_TAG: i64 = 4;

/// One Workset as shown in the status menu.
#[derive(Debug, Clone)]
pub struct WorksetMenuEntry {
    pub name: String,
    /// The key that selects this Workset, without its modifiers.
    pub key_equivalent: Option<String>,
    pub is_active: bool,
}

pub struct StatusIcon {
    status_item: Retained<NSStatusItem>,
    mtm: MainThreadMarker,
    _menu_handler: Retained<MenuHandler>,
    toggle_item: Retained<NSMenuItem>,
    space_toggle_item: Retained<NSMenuItem>,
    /// Menu items naming this Space's Worksets, rebuilt whenever they change.
    workset_items: Vec<Retained<NSMenuItem>>,
    /// Where the Workset items start, so they can be replaced in place.
    workset_index: usize,
}

impl StatusIcon {
    /// Creates a new menu bar manager.
    pub fn new(
        config: &config::StatusIconExperimental,
        mtm: MainThreadMarker,
        wm_tx: wm_controller::Sender,
    ) -> Self {
        let status_bar = NSStatusBar::systemStatusBar();
        let status_item = status_bar.statusItemWithLength(NSVariableStatusItemLength);

        // Create parachute icon
        if let Some(button) = status_item.button(mtm)
            && let Some(parachute_image) = create_parachute_icon(config)
        {
            button.setImage(Some(&parachute_image));
        }

        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("Glide"));

        // This is needed to be able to manually set the menu item state
        menu.setAutoenablesItems(false);

        let menu_handler = MenuHandler::new(mtm, wm_tx);

        let space_toggle_ns_title = ns_string!("Enable Space");
        let space_toggle_item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &space_toggle_ns_title,
                Some(sel!(handleAction:)),
                ns_string!(""),
            )
        };
        unsafe { space_toggle_item.setTarget(Some(&*menu_handler)) };
        space_toggle_item.setTag(TOGGLE_SPACE_TAG as isize);
        menu.addItem(&space_toggle_item);

        menu.addItem(&NSMenuItem::separatorItem(mtm));
        let workset_index = menu.numberOfItems() as usize;

        let version_item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(&format!("Glide v{}", env!("CARGO_PKG_VERSION"))),
                None,
                ns_string!(""),
            )
        };
        version_item.setEnabled(false);
        menu.addItem(&version_item);

        let docs_item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                ns_string!("Documentation"),
                Some(sel!(handleAction:)),
                ns_string!(""),
            )
        };
        unsafe { docs_item.setTarget(Some(&*menu_handler)) };
        docs_item.setTag(SHOW_DOCS_TAG as isize);
        menu.addItem(&docs_item);

        menu.addItem(&NSMenuItem::separatorItem(mtm));

        let toggle_ns_title = ns_string!("Enable Glide");
        let toggle_item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &toggle_ns_title,
                Some(sel!(handleAction:)),
                ns_string!(""),
            )
        };
        unsafe { toggle_item.setTarget(Some(&*menu_handler)) };
        toggle_item.setTag(TOGGLE_GLOBAL_TAG as isize);
        menu.addItem(&toggle_item);

        let save_quit_item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                ns_string!("Quit"),
                Some(sel!(handleAction:)),
                ns_string!(""),
            )
        };
        unsafe { save_quit_item.setTarget(Some(&*menu_handler)) };
        save_quit_item.setTag(SAVE_AND_QUIT_TAG as isize);
        menu.addItem(&save_quit_item);

        status_item.setMenu(Some(&menu));

        Self {
            status_item,
            mtm,
            _menu_handler: menu_handler,
            toggle_item,
            space_toggle_item,
            workset_items: Vec::new(),
            workset_index,
        }
    }

    /// Lists this Space's Worksets in the menu, marking the active one and
    /// showing the shortcut that selects each.
    ///
    /// Worksets are created as they are used, so the list is rebuilt rather
    /// than updated in place.
    pub fn set_worksets(&mut self, worksets: &[WorksetMenuEntry]) {
        let Some(menu) = self.status_item.menu(self.mtm) else {
            return;
        };
        for item in self.workset_items.drain(..) {
            menu.removeItem(&item);
        }
        for (offset, entry) in worksets.iter().enumerate() {
            // A bullet marks where the user is. NSMenuItem's own checkmark
            // state is not exposed here, and a prefix reads the same. The
            // shortcut goes in the title because a disabled item does not
            // render a key equivalent.
            let title = NSString::from_str(&format!(
                "{} {}{}",
                if entry.is_active { "●" } else { "○" },
                entry.name,
                match &entry.key_equivalent {
                    Some(key) => format!("  ⌥{}", key.to_uppercase()),
                    None => String::new(),
                },
            ));
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(self.mtm),
                    &title,
                    None,
                    ns_string!(""),
                )
            };
            // A tick marks where the user is; the rest are shown for
            // orientation, not as actions, so they stay disabled.
            item.setEnabled(false);
            menu.insertItem_atIndex(&item, (self.workset_index + offset) as isize);
            self.workset_items.push(item);
        }
    }

    /// Sets the text next to the icon.
    pub fn set_text(&mut self, text: &str) {
        let ns_title = NSString::from_str(text);
        if let Some(button) = self.status_item.button(self.mtm) {
            button.setTitle(&ns_title);
        } else {
            warn!("Could not get button from status item");
        }
    }

    /// Sets the toggle menu item title.
    pub fn set_toggle_title(&mut self, title: &str) {
        let ns_title = NSString::from_str(title);
        self.toggle_item.setTitle(&ns_title);
    }

    /// Sets the space toggle menu item title.
    pub fn set_space_toggle_title(&mut self, title: &str) {
        let ns_title = NSString::from_str(title);
        self.space_toggle_item.setTitle(&ns_title);
    }

    /// Sets whether the space toggle menu item is enabled.
    pub fn set_space_toggle_enabled(&mut self, enabled: bool) {
        self.space_toggle_item.setEnabled(enabled);
    }
}

impl Drop for StatusIcon {
    fn drop(&mut self) {
        debug!("Removing menu bar icon");
        let status_bar = NSStatusBar::systemStatusBar();
        status_bar.removeStatusItem(&self.status_item);
    }
}

struct MenuHandlerIvars {
    wm_tx: wm_controller::Sender,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = MenuHandlerIvars]
    struct MenuHandler;

    impl MenuHandler {
        #[unsafe(method(handleAction:))]
        fn handle_action(&self, sender: &NSObject) {
            let tag = unsafe {
                let tag: i64 = msg_send![sender, tag];
                tag
            };
            debug!("Menu action received with tag: {}", tag);
            let wm_tx = &self.ivars().wm_tx;
            match tag {
                SAVE_AND_QUIT_TAG => {
                    debug!("Sending SaveAndExit command");
                    let _ = wm_tx.send((
                        Span::current(),
                        WmEvent::Command(WmCommand::ReactorCommand(
                            reactor::Command::Reactor(reactor::ReactorCommand::SaveAndExit),
                        )),
                    ));
                }
                TOGGLE_GLOBAL_TAG => {
                    debug!("Sending ToggleGlobalEnabled command");
                    let _ = wm_tx.send((
                        Span::current(),
                        WmEvent::Command(WmCommand::Wm(WmCmd::ToggleGlobalEnabled)),
                    ));
                }
                TOGGLE_SPACE_TAG => {
                    debug!("Sending ToggleSpaceActivated command");
                    let _ = wm_tx.send((
                        Span::current(),
                        WmEvent::Command(WmCommand::Wm(WmCmd::ToggleSpaceActivated)),
                    ));
                }
                SHOW_DOCS_TAG => {
                    debug!("Opening docs in browser");
                    if let Err(e) = std::process::Command::new("/usr/bin/open")
                        .arg("https://glidewm.org/reference/config")
                        .spawn()
                    {
                        error!("Failed to open documentation: {e}");
                    }
                }
                _ => {
                    warn!("Unknown tag: {}", tag);
                }
            }
        }
    }
);

impl MenuHandler {
    /// Creates the parachute icon from the SVG file
    pub fn new(mtm: MainThreadMarker, wm_tx: wm_controller::Sender) -> Retained<Self> {
        let this = Self::alloc(mtm);
        let this = this.set_ivars(MenuHandlerIvars { wm_tx });
        unsafe { msg_send![super(this), init] }
    }
}

fn create_parachute_icon(config: &config::StatusIconExperimental) -> Option<Retained<NSImage>> {
    // Load the SVG file
    let svg_data = if config.color {
        include_str!("../../site/src/assets/parachute-small.svg")
    } else {
        include_str!("../../site/src/assets/parachute-nocolor.svg")
    };
    let ns_data =
        unsafe { NSData::dataWithBytes_length(svg_data.as_ptr() as *const c_void, svg_data.len()) };

    let Some(image) = NSImage::initWithData(NSImage::alloc(), &ns_data) else {
        return None;
    };

    // Set the image size to be appropriate for menu bar (16x16 points)
    image.setSize(CGSize { width: 16.0, height: 16.0 });

    if !config.color {
        // Set as template image so it follows system appearance
        image.setTemplate(true);
    }

    Some(image)
}
