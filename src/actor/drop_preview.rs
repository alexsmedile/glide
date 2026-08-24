// Copyright The Glide Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Displays the destination of an in-progress mouse layout operation.

use objc2::MainThreadOnly;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSFloatingWindowLevel, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_core_foundation::CGRect;
use objc2_foundation::{MainThreadMarker, NSZeroRect};
use objc2_quartz_core::CALayer;

use crate::actor;
use crate::actor::layout::{WindowDropPlacement, WindowDropPreview};
use crate::sys::screen::CoordinateConverter;

#[derive(Debug)]
pub enum Event {
    Show(WindowDropPreview),
    Hide,
    ScreenParametersChanged(CoordinateConverter),
}

pub type Sender = actor::Sender<Event>;
pub type Receiver = actor::Receiver<Event>;

pub struct DropPreview {
    rx: Receiver,
    window: Retained<NSWindow>,
    layer: Retained<CALayer>,
    converter: CoordinateConverter,
}

impl DropPreview {
    pub fn new(rx: Receiver, mtm: MainThreadMarker) -> Self {
        let window = make_window(mtm);
        let view = NSView::initWithFrame(NSView::alloc(mtm), CGRect::ZERO);
        view.setWantsLayer(true);
        let layer = CALayer::layer();
        layer.setCornerRadius(10.0);
        layer.setBorderWidth(2.0);
        view.setLayer(Some(&layer));
        window.setContentView(Some(&view));

        Self {
            rx,
            window,
            layer,
            converter: CoordinateConverter::default(),
        }
    }

    pub async fn run(mut self) {
        while let Some((span, event)) = self.rx.recv().await {
            let _guard = span.enter();
            self.handle_event(event);
        }
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Show(preview) => self.show(preview),
            Event::Hide => self.window.orderOut(None),
            Event::ScreenParametersChanged(converter) => self.converter = converter,
        }
    }

    fn show(&self, preview: WindowDropPreview) {
        let Some(frame) = self.converter.convert_rect(preview.frame) else {
            self.window.orderOut(None);
            return;
        };
        let (fill, border) = colors(preview.placement);
        self.layer.setBackgroundColor(Some(&fill.CGColor()));
        self.layer.setBorderColor(Some(&border.CGColor()));
        self.window.setFrame_display(frame, false);
        self.window.orderFrontRegardless();
    }
}

impl Drop for DropPreview {
    fn drop(&mut self) {
        self.window.close();
    }
}

fn colors(placement: WindowDropPlacement) -> (Retained<NSColor>, Retained<NSColor>) {
    let (red, green, blue) = match placement {
        WindowDropPlacement::Group => (0.25, 0.78, 0.48),
        WindowDropPlacement::SplitLeft
        | WindowDropPlacement::SplitRight
        | WindowDropPlacement::SplitTop
        | WindowDropPlacement::SplitBottom => (0.55, 0.38, 0.95),
        _ => (0.10, 0.56, 0.96),
    };
    (
        NSColor::colorWithRed_green_blue_alpha(red, green, blue, 0.24),
        NSColor::colorWithRed_green_blue_alpha(red, green, blue, 0.90),
    )
}

fn make_window(mtm: MainThreadMarker) -> Retained<NSWindow> {
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSZeroRect,
            NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            true,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setLevel(NSFloatingWindowLevel);
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    window.setOpaque(false);
    window.setHasShadow(false);
    window.setIgnoresMouseEvents(true);
    window
}
