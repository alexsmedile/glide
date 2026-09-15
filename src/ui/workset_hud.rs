// Copyright The Glide Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Transient overlay naming the Workset that was just activated.
//!
//! Worksets share a Space, so switching between them changes which windows
//! are on screen without any of the cues a Space switch gives. The overlay
//! is that cue: it names the Workset, then fades out on its own.

use objc2::MainThreadOnly;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSFont, NSScreen, NSTextAlignment, NSTextField, NSView, NSWindow,
    NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{MainThreadMarker, NSString};

/// Size of the overlay panel.
const SIZE: CGSize = CGSize { width: 260.0, height: 96.0 };
/// Corner radius of the panel's rounded rectangle.
const CORNER_RADIUS: f64 = 18.0;

/// A reusable overlay panel that names the active Workset.
pub struct WorksetHud {
    window: Retained<NSWindow>,
    label: Retained<NSTextField>,
}

impl WorksetHud {
    pub fn new(mtm: MainThreadMarker) -> Self {
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                CGRect {
                    origin: CGPoint { x: 0.0, y: 0.0 },
                    size: SIZE,
                },
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                true,
            )
        };
        // Matches the group indicator: closing a window that is still
        // referenced here would otherwise double release.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setOpaque(false);
        window.setIgnoresMouseEvents(true);
        // Above normal windows but below the group bars, so it never hides a
        // layout cue the user is acting on.
        window.setLevel(3);

        let backdrop = {
            let view = NSView::initWithFrame(
                NSView::alloc(mtm),
                CGRect {
                    origin: CGPoint { x: 0.0, y: 0.0 },
                    size: SIZE,
                },
            );
            view.setWantsLayer(true);
            if let Some(layer) = view.layer() {
                layer.setCornerRadius(CORNER_RADIUS);
                layer.setMasksToBounds(true);
                let backing =
                    NSColor::colorWithCalibratedRed_green_blue_alpha(0.08, 0.08, 0.10, 0.92);
                layer.setBackgroundColor(Some(&backing.CGColor()));
            }
            view
        };

        let label = {
            let field = NSTextField::labelWithString(&NSString::from_str(""), mtm);
            field.setAlignment(NSTextAlignment::Center);
            field.setTextColor(Some(&NSColor::whiteColor()));
            field.setFont(Some(&NSFont::boldSystemFontOfSize(28.0)));
            field.setFrame(CGRect {
                origin: CGPoint { x: 0.0, y: 30.0 },
                size: CGSize {
                    width: SIZE.width,
                    height: 40.0,
                },
            });
            field
        };
        backdrop.addSubview(&label);
        window.setContentView(Some(&backdrop));

        WorksetHud { window, label }
    }

    /// Shows the overlay naming `workset`, centered on the main screen.
    pub fn show(&self, workset: &str, mtm: MainThreadMarker) {
        self.label.setStringValue(&NSString::from_str(workset));
        if let Some(screen) = NSScreen::mainScreen(mtm) {
            let visible = screen.visibleFrame();
            let origin = CGPoint {
                x: visible.origin.x + (visible.size.width - SIZE.width) / 2.0,
                y: visible.origin.y + (visible.size.height - SIZE.height) / 2.0,
            };
            self.window.setFrameOrigin(origin);
        }
        self.window.orderFront(None);
        self.window.setAlphaValue(1.0);
    }

    /// Hides the overlay.
    pub fn hide(&self) {
        self.window.orderOut(None);
    }
}
