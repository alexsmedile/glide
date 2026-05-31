// Copyright The Glide Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Standalone demo of yabai-style "proxy window" animation, adapted for Glide.
//!
//! Background: the Accessibility API is the only sanctioned way to move another
//! app's window, but it is slow and synchronous, so it cannot drive a smooth
//! 60/120 Hz animation. yabai sidesteps this by animating a *screenshot* of the
//! window on a private window-server layer (a "proxy"), moving the real window
//! once while it is hidden, then swapping back. See the report accompanying this
//! example for the full breakdown of yabai's implementation.
//!
//! What this demo does differently from yabai: yabai hides the real window with
//! the privileged `SLSSetWindowSystemAlpha` (which it can only call from a
//! payload injected into Dock.app). We avoid that entirely. Instead we lay an
//! opaque **backdrop** snapshot of *everything behind the window* over the area
//! the animation touches, so the real window is occluded without any privileged
//! call. The backdrop is captured with `CGWindowListCreateImage` using
//! `kCGWindowListOptionOnScreenBelowWindow`, which composites the desktop and
//! other windows *excluding the target window itself* — exactly the "desktop
//! behind the window" image we need.
//!
//! Pipeline:
//!   1. Capture the target window image (private `SLSHWCaptureWindowList`, as
//!      yabai does) → the proxy bitmap.
//!   2. Capture the backdrop (everything below the window, in the union of the
//!      origin and destination rects).
//!   3. Create server-owned windows on a dedicated SLS connection: the backdrop
//!      (static, opaque) and the proxy (animated) above it. Creating them puts
//!      them at the front, so re-activate the app that was focused when we
//!      started — its key window returns above our proxy and stays live, which
//!      keeps the window the user is working in visible (we can't order our
//!      window below a foreign one at the same layer without privilege).
//!   4. Move the real window to its destination *instantly* via AX — invisible,
//!      because the backdrop covers it.
//!   5. Animate the proxy's position *and* size from
//!      `model::spring::SpringAnimation` with one `SLSSetWindowTransform` per
//!      frame (yabai's recipe: translate by `-cur`, scale by `origin/cur`),
//!      paced by a `CVDisplayLink` (vsync-locked).
//!   6. On settle, tear the proxy + backdrop down atomically; the real window
//!      (now at the destination) shows through.
//!
//! Two macOS quirks shape the structure, both discovered the hard way:
//!   * **SLS connections are thread-affine.** The connection is created on the
//!     main thread, so every window call must run there. The CVDisplayLink
//!     callback fires on its own thread, so it does no SLS work — it just ticks
//!     the main thread once per vsync through a dispatch semaphore.
//!   * **Main-thread SLS calls only flush when the run loop turns.** Without a
//!     run-loop turn the transform silently has no visible effect (this looked
//!     at first like a privilege problem, but it is just buffering). So the
//!     per-frame loop spins the run loop briefly after setting the transform.
//!
//! Run it:
//!   cargo run --example proxy_animation                 # spawns its own window
//!   cargo run --example proxy_animation -- --wid 12345  # animates an existing window
//!
//! Requires **Screen Recording** permission (for the captures). The demo checks
//! and prompts for it on launch.

use std::ffi::{c_int, c_void};
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use accessibility::{AXUIElement, AXUIElementAttributes};
use accessibility_sys::pid_t;
use core_graphics_types::geometry as cg;
use glide_wm::model::spring::SpringAnimation;
use glide_wm::sys::app::AXUIElementExt;
use glide_wm::sys::window_server::{self, WindowServerId};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSFont, NSScreen, NSTextAlignment, NSTextField, NSView, NSWindow, NSWindowAnimationBehavior, NSWindowOrderingMode, NSWindowStyleMask
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGImage;
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use objc2_quartz_core::{CALayer, CATransaction};

// ---------------------------------------------------------------------------
// Private/undocumented FFI. These mirror the declarations yabai uses
// (`yabai/src/misc/extern.h`). They rely on private SkyLight APIs and are not
// guaranteed across macOS versions.
// ---------------------------------------------------------------------------

type SLSConnectionID = c_int;
type CGWindowID = u32;
type CFTypeRef = *const c_void;
type CFArrayRef = *const c_void;
type CGImageRef = *const c_void;
type CGWindowImageOption = u32;
type CGWindowListOption = u32;

type CVDisplayLinkRef = *mut c_void;
type CVReturn = i32;
type CVOptionFlags = u64;

const KCG_WINDOW_LIST_OPTION_ON_SCREEN_BELOW_WINDOW: CGWindowListOption = 1 << 2;
const KCG_WINDOW_IMAGE_DEFAULT: CGWindowImageOption = 0;
const KCV_RETURN_SUCCESS: CVReturn = 0;

// SLSHWCaptureWindowList flags used by yabai.
const SLS_CAPTURE_FLAGS: u32 = (1 << 11) | (1 << 8);

// The only remaining private SkyLight calls are read-only window queries and the
// hardware window capture used to snapshot the target. Window *creation*,
// ordering, levels, shadow, and the per-frame transform are now done with public
// AppKit/Core Animation APIs (see `ProxyWindow`).
#[link(name = "SkyLight", kind = "framework")]
unsafe extern "C" {
    fn SLSNewConnection(zero: c_int, cid: *mut SLSConnectionID) -> i32;
    fn SLSReleaseConnection(cid: SLSConnectionID) -> i32;

    fn SLSGetWindowBounds(cid: SLSConnectionID, wid: CGWindowID, frame: *mut CGRect) -> i32;
    fn SLSGetWindowLevel(cid: SLSConnectionID, wid: CGWindowID, level: *mut c_int) -> i32;
    fn SLSGetWindowSubLevel(cid: SLSConnectionID, wid: CGWindowID) -> c_int;

    fn SLSHWCaptureWindowList(
        cid: SLSConnectionID,
        window_list: *const CGWindowID,
        window_count: c_int,
        options: u32,
    ) -> CFArrayRef;
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGWindowListCreateImage(
        bounds: CGRect,
        list_option: CGWindowListOption,
        window_id: CGWindowID,
        image_option: CGWindowImageOption,
    ) -> CGImageRef;
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
    fn CGMainDisplayID() -> CGDirectDisplayID;
    fn CGGetDisplaysWithPoint(
        point: CGPoint,
        max_displays: u32,
        displays: *mut CGDirectDisplayID,
        matching_count: *mut u32,
    ) -> i32;
    fn CGGetActiveDisplayList(
        max_displays: u32,
        displays: *mut CGDirectDisplayID,
        matching_count: *mut u32,
    ) -> i32;
    fn CGDisplayBounds(display: CGDirectDisplayID) -> CGRect;
    fn CGDisplayIsBuiltin(display: CGDirectDisplayID) -> i32;
}

type CGDirectDisplayID = u32;

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    fn CVDisplayLinkCreateWithCGDisplay(
        display: CGDirectDisplayID,
        link: *mut CVDisplayLinkRef,
    ) -> CVReturn;
    #[allow(dead_code)]
    fn CVDisplayLinkCreateWithActiveCGDisplays(link: *mut CVDisplayLinkRef) -> CVReturn;
    fn CVDisplayLinkSetOutputCallback(
        link: CVDisplayLinkRef,
        callback: CVDisplayLinkOutputCallback,
        user_info: *mut c_void,
    ) -> CVReturn;
    fn CVDisplayLinkStart(link: CVDisplayLinkRef) -> CVReturn;
    fn CVDisplayLinkStop(link: CVDisplayLinkRef) -> CVReturn;
    fn CVDisplayLinkRelease(link: CVDisplayLinkRef);
    fn CVDisplayLinkGetNominalOutputVideoRefreshPeriod(link: CVDisplayLinkRef) -> CVTime;
}

#[repr(C)]
struct CVTime {
    time_value: i64,
    time_scale: i32,
    flags: i32,
}

// libdispatch (in libSystem, linked by default) — used to tick the main thread
// from the CVDisplayLink thread, since the SLS connection is thread-affine.
type DispatchSemaphore = *mut c_void;
const DISPATCH_TIME_NOW: u64 = 0;

unsafe extern "C" {
    fn dispatch_semaphore_create(value: isize) -> DispatchSemaphore;
    fn dispatch_semaphore_signal(sem: DispatchSemaphore) -> isize;
    fn dispatch_semaphore_wait(sem: DispatchSemaphore, timeout: u64) -> isize;
    fn dispatch_time(when: u64, delta: i64) -> u64;
    fn dispatch_release(obj: *mut c_void);
}

type CVDisplayLinkOutputCallback = extern "C" fn(
    link: CVDisplayLinkRef,
    now: *const c_void,
    output_time: *const c_void,
    flags_in: CVOptionFlags,
    flags_out: *mut CVOptionFlags,
    user_info: *mut c_void,
) -> CVReturn;

type CFStringRef = *const c_void;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: CFTypeRef);
    fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    fn CFArrayGetCount(array: CFArrayRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CFArrayRef, idx: isize) -> *const c_void;
    fn CFRunLoopRunInMode(mode: CFStringRef, seconds: f64, return_after_handled: bool) -> i32;
    static kCFRunLoopDefaultMode: CFStringRef;
}

/// Pump the main run loop for roughly `seconds`, so AppKit renders and the
/// window server has a chance to composite before we screenshot.
fn pump_runloop(seconds: f64) {
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < deadline {
        unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.02, true) };
    }
}

// ---------------------------------------------------------------------------
// A plain AppKit `NSWindow` holding a bitmap (the proxy or the backdrop).
//
// This is the heart of the migration away from server-owned SLS windows. The
// window server already stacks an *inactive* app's windows below the *active*
// app's windows, so a non-activating proxy owned by glide (an accessory app)
// naturally sits behind whatever window the user is working in — and that
// foreground window keeps rendering live on top. That is exactly the ordering
// we could never get by hand-placing a private SLS window.
//
// We never change the active app or the key window: windows are ordered in with
// `orderFront` (which, for a background app, lands above other inactive windows
// but below the active app), and `orderWindow:relativeTo:` is used *only* to
// stack the proxy above the backdrop — both ours. (That call cannot reorder a
// foreign window, which is why it can't be used to climb above the foreground.)
// ---------------------------------------------------------------------------

struct ProxyWindow {
    window: Retained<NSWindow>,
    layer: Retained<CALayer>,
}

impl ProxyWindow {
    /// Create a borderless, non-activating window at `frame` (CG global coords)
    /// showing `image`, at window level `level`.
    fn new(
        mtm: MainThreadMarker,
        frame: CGRect,
        image: CGImageRef,
        level: c_int,
        opaque: bool,
    ) -> Self {
        assert!(!image.is_null(), "cannot build a proxy from a null image");
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                cg_to_ns_rect(frame),
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        window.setLevel(level as isize);
        window.setOpaque(opaque);
        window.setHasShadow(false);
        // Disable AppKit's automatic open/close scale+fade. Without this the
        // proxy/backdrop "zoom" in and out, and during the fade-in the backdrop
        // is briefly translucent, revealing the relocated real window's edge.
        window.setAnimationBehavior(NSWindowAnimationBehavior::None);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setIgnoresMouseEvents(true);
        unsafe { window.setReleasedWhenClosed(false) };

        // Layer-backed content view; the bitmap lives in the layer and scales
        // with the window (replacing yabai's per-frame SLSSetWindowTransform).
        let content: Retained<NSView> = window.contentView().expect("content view");
        content.setWantsLayer(true);
        let layer = content.layer().expect("layer-backed view has a layer");
        layer.setContentsGravity(&NSString::from_str("resize"));
        set_layer_image(&layer, image);

        // Order in without activating glide (background app -> above other
        // inactive windows, below the active app).
        window.orderFront(None);

        ProxyWindow { window, layer }
    }

    /// Stack this window directly above `other` (must be one of *our* windows;
    /// AppKit relative ordering does not work across applications).
    fn order_above(&self, other: &ProxyWindow) {
        self.window
            .orderWindow_relativeTo(NSWindowOrderingMode::Above, other.window.windowNumber());
    }

    fn without_shadow(self) -> Self {
        // Shadow is already disabled in `new`; retained for call-site parity.
        self
    }

    /// Swap the layer's bitmap for a freshly captured frame. The caller must be
    /// inside a no-actions `CATransaction` so the swap is presented atomically
    /// with the frame change (see `run_spring`).
    fn set_contents(&self, image: CGImageRef) {
        if image.is_null() {
            return;
        }
        unsafe {
            let obj: &AnyObject = &*(image as *const AnyObject);
            self.layer.setContents(Some(obj));
        }
    }

    /// Move and resize the window to `frame` (CG global coords). Replaces the
    /// per-frame SLS transform; the layer's `resize` gravity scales the bitmap.
    /// The caller must be inside a no-actions `CATransaction`: otherwise
    /// resizing the window starts an implicit Core Animation on the backing
    /// layer's bounds, which lags the window by a frame and scales the contents
    /// wrong. Disabling actions keeps the layer locked to the window.
    fn set_frame(&self, frame: CGRect) {
        self.window.setFrame_display(cg_to_ns_rect(frame), false);
    }

    /// The window-server id, for debug dumps.
    fn id(&self) -> CGWindowID {
        self.window.windowNumber() as CGWindowID
    }

    fn hide(&self) {
        self.window.orderOut(None);
    }

    fn show(&self) {
        self.window.orderFront(None);
    }
}

/// Assign a `CGImageRef` as a layer's contents with implicit animations off, so
/// per-frame swaps don't cross-fade. The layer retains the image itself.
fn set_layer_image(layer: &CALayer, image: CGImageRef) {
    unsafe {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        let obj: &AnyObject = &*(image as *const AnyObject);
        layer.setContents(Some(obj));
        CATransaction::commit();
    }
}

/// Convert a CG global rect (top-left origin, y down) to an AppKit screen rect
/// (bottom-left origin, y up) by flipping through the primary display's height.
fn cg_to_ns_rect(r: CGRect) -> NSRect {
    let primary_h = unsafe { CGDisplayBounds(CGMainDisplayID()) }.size.height;
    NSRect::new(
        NSPoint::new(r.origin.x, primary_h - (r.origin.y + r.size.height)),
        NSSize::new(r.size.width, r.size.height),
    )
}

/// A `CGImageRef` that can be sent between threads and releases on drop. Used to
/// hand freshly captured frames from the capture thread to the animation loop.
/// Carries the captured pixel size so the loop can reject frames grabbed while
/// the real window was mid-resize (which render partly blank).
struct SendImage {
    img: CGImageRef,
    px: (usize, usize),
}
unsafe impl Send for SendImage {}
impl Drop for SendImage {
    fn drop(&mut self) {
        if !self.img.is_null() {
            unsafe { CFRelease(self.img) };
        }
    }
}

impl Drop for ProxyWindow {
    fn drop(&mut self) {
        self.window.orderOut(None);
        self.window.close();
    }
}

// ---------------------------------------------------------------------------
// CVDisplayLink callback: it runs on a *separate* thread, but the SLS
// connection is thread-affine to the main thread. So the callback does no SLS
// work at all — it just ticks the main thread (which owns the connection) once
// per vsync via a dispatch semaphore. The main thread renders each frame.
// ---------------------------------------------------------------------------

extern "C" fn display_link_callback(
    _link: CVDisplayLinkRef,
    _now: *const c_void,
    _output_time: *const c_void,
    _flags_in: CVOptionFlags,
    _flags_out: *mut CVOptionFlags,
    user_info: *mut c_void,
) -> CVReturn {
    unsafe { dispatch_semaphore_signal(user_info as DispatchSemaphore) };
    KCV_RETURN_SUCCESS
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

fn lerp_rect(a: CGRect, b: CGRect, t: f64) -> CGRect {
    CGRect::new(
        CGPoint::new(lerp(a.origin.x, b.origin.x, t), lerp(a.origin.y, b.origin.y, t)),
        CGSize::new(
            lerp(a.size.width, b.size.width, t),
            lerp(a.size.height, b.size.height, t),
        ),
    )
}

fn union_rect(a: CGRect, b: CGRect) -> CGRect {
    let min_x = a.origin.x.min(b.origin.x);
    let min_y = a.origin.y.min(b.origin.y);
    let max_x = (a.origin.x + a.size.width).max(b.origin.x + b.size.width);
    let max_y = (a.origin.y + a.size.height).max(b.origin.y + b.size.height);
    CGRect::new(
        CGPoint::new(min_x, min_y),
        CGSize::new(max_x - min_x, max_y - min_y),
    )
}

fn inflate(r: CGRect, m: f64) -> CGRect {
    CGRect::new(
        CGPoint::new(r.origin.x - m, r.origin.y - m),
        CGSize::new(r.size.width + 2.0 * m, r.size.height + 2.0 * m),
    )
}

/// Clamp `rect` so it lies entirely within `bounds`: shrink it to fit if it is
/// larger than `bounds` in a dimension, then shift it inward. The proxy is a
/// screen capture, so any part of the window past the display edge captures as
/// blank — clamping keeps the whole window on screen and the animation clean.
fn clamp_to_rect(mut rect: CGRect, bounds: CGRect) -> CGRect {
    rect.size.width = rect.size.width.min(bounds.size.width);
    rect.size.height = rect.size.height.min(bounds.size.height);
    let max_x = bounds.origin.x + bounds.size.width - rect.size.width;
    let max_y = bounds.origin.y + bounds.size.height - rect.size.height;
    rect.origin.x = rect.origin.x.clamp(bounds.origin.x, max_x);
    rect.origin.y = rect.origin.y.clamp(bounds.origin.y, max_y);
    rect
}

/// macOS window drop shadows extend well beyond the window frame. The real
/// window is moved to its destination immediately (hidden behind the backdrop),
/// but its shadow would otherwise spill past the backdrop edge at the new
/// location before the proxy arrives. Pad the backdrop to swallow that halo.
/// (The shadow is excluded from the below-window capture, so the padded
/// backdrop just shows more of the desktop behind it.)
const SHADOW_MARGIN: f64 = 64.0;

// ---------------------------------------------------------------------------
// Capture helpers.
// ---------------------------------------------------------------------------

/// Capture the target window itself (the proxy bitmap), via the private SLS
/// hardware capture path yabai uses. Returns a +1-retained `CGImageRef`.
fn capture_window(cid: SLSConnectionID, wid: CGWindowID) -> Option<CGImageRef> {
    unsafe {
        let array = SLSHWCaptureWindowList(cid, &wid, 1, SLS_CAPTURE_FLAGS);
        if array.is_null() || CFArrayGetCount(array) < 1 {
            if !array.is_null() {
                CFRelease(array);
            }
            return None;
        }
        let image = CFArrayGetValueAtIndex(array, 0);
        let retained = CFRetain(image);
        CFRelease(array);
        Some(retained)
    }
}

/// Capture everything *behind* the window in `bounds` (window excluded), to use
/// as the occluding backdrop. Returns a +1-retained `CGImageRef`.
fn capture_backdrop(bounds: CGRect, wid: CGWindowID) -> Option<CGImageRef> {
    capture_relative_to_window(bounds, wid, KCG_WINDOW_LIST_OPTION_ON_SCREEN_BELOW_WINDOW)
}

fn capture_relative_to_window(
    bounds: CGRect,
    wid: CGWindowID,
    option: CGWindowListOption,
) -> Option<CGImageRef> {
    let image = unsafe { CGWindowListCreateImage(bounds, option, wid, KCG_WINDOW_IMAGE_DEFAULT) };
    if image.is_null() { None } else { Some(image) }
}

// ---------------------------------------------------------------------------
// The animation itself.
// ---------------------------------------------------------------------------

struct Target {
    wsid: WindowServerId,
    elem: AXUIElement,
}

/// Resolve an existing window id to an animatable target.
fn resolve_existing(wsid: WindowServerId) -> Option<Target> {
    let info = window_server::get_window(wsid)?;
    let elem = AXUIElement::application(info.pid)
        .windows()
        .ok()?
        .iter()
        .find(|w| WindowServerId::try_from(&**w).ok() == Some(wsid))
        .map(|w| w.clone())?;
    Some(Target { wsid, elem })
}

/// Run the full proxy-window animation for `target`, moving it to `dest`.
/// Blocks until the animation settles, then tears everything down.
fn animate(target: &Target, dest: CGRect) {
    let mtm = MainThreadMarker::new().expect("animate must run on the main thread");

    // Run as an accessory (background) app and never activate. That is what lets
    // `orderFront` place the proxy/backdrop above other *inactive* windows yet
    // below the *active* app's windows — so the window the user is working in
    // stays on top and keeps receiving input, with no reactivation dance.
    NSApplication::sharedApplication(mtm)
        .setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let wid = target.wsid.as_u32();

    let mut cid: SLSConnectionID = 0;
    unsafe { SLSNewConnection(0, &mut cid) };

    // Current frame straight from the window server (CG global coords).
    let mut origin = CGRect::default();
    unsafe { SLSGetWindowBounds(cid, wid, &mut origin) };

    let mut level: c_int = 0;
    unsafe { SLSGetWindowLevel(cid, wid, &mut level) };

    println!("origin = {origin:?}");
    println!("dest   = {dest:?}");

    let backdrop_bounds = inflate(union_rect(origin, dest), SHADOW_MARGIN);

    // 1 + 2: captures. Backdrop first, while the window is still at the origin.
    let Some(backdrop_img) = capture_backdrop(backdrop_bounds, wid) else {
        eprintln!("backdrop capture failed (Screen Recording permission?)");
        unsafe { SLSReleaseConnection(cid) };
        return;
    };
    let Some(proxy_img) = capture_window(cid, wid) else {
        eprintln!("window capture failed (Screen Recording permission?)");
        unsafe {
            CFRelease(backdrop_img);
            SLSReleaseConnection(cid);
        }
        return;
    };
    let debug = std::env::var("GLIDE_DEBUG").is_ok();
    if debug {
        dump_z_order(target.wsid);
    }

    // Pixels-per-point of the capture (≈2 on Retina). Used to predict the pixel
    // size a *settled* live capture should have at each end of the animation, so
    // the loop can drop mid-resize captures (which come back a wrong size and
    // render partly blank — the black flash).
    let px_scale = {
        let c = unsafe { &*(proxy_img as *const CGImage) };
        CGImage::width(Some(c)) as f64 / origin.size.width
    };
    let expected_px = |r: CGRect| {
        (
            (r.size.width * px_scale).round() as usize,
            (r.size.height * px_scale).round() as usize,
        )
    };

    // 3: create the windows: backdrop (opaque, hides the relocated real target)
    // and the proxy (animated) above it. Both are ordered in with `orderFront`
    // (background-app ordering keeps them below the active app), and the proxy
    // is stacked directly above the backdrop with `orderWindow:relativeTo:`.
    let backdrop =
        ProxyWindow::new(mtm, backdrop_bounds, backdrop_img, level, true).without_shadow();
    let proxy = ProxyWindow::new(mtm, origin, proxy_img, level, false);
    proxy.order_above(&backdrop);

    if debug {
        dump_level(cid, "target  ", wid);
        dump_level(cid, "backdrop", backdrop.id());
        dump_level(cid, "proxy   ", proxy.id());
        dump_z_order(target.wsid);
    }

    // Live content: a background thread captures the target window on its *own*
    // SLS connection (connections are thread-affine) and atomically swaps the
    // latest frame into `live`; the animation loop redraws the proxy from it.
    let live: Arc<Mutex<Option<SendImage>>> = Arc::new(Mutex::new(None));
    let stop = Arc::new(AtomicBool::new(false));
    let capture_thread = {
        let live = live.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut cap_cid: SLSConnectionID = 0;
            unsafe { SLSNewConnection(0, &mut cap_cid) };
            let mut last_frame = None;
            while !stop.load(Ordering::Relaxed) {
                if let Some(img) = capture_window(cap_cid, wid) {
                    // Record the captured pixel size so the animation loop can
                    // skip mid-resize frames (see `run_spring`).
                    let cap = unsafe { Retained::retain(img as *mut CGImage) }.unwrap();
                    let px = (CGImage::width(Some(&cap)), CGImage::height(Some(&cap)));
                    if last_frame != Some(px) {
                        dbg!(px);
                    }
                    last_frame.replace(px);

                    *live.lock().unwrap() = Some(SendImage { img, px });
                }
                std::thread::sleep(Duration::from_millis(6));
            }
            unsafe { SLSReleaseConnection(cap_cid) };
        })
    };

    // 5: drive the proxy on the MAIN thread (which owns the SLS connection),
    // paced by a CVDisplayLink on the *window's* display, so a ProMotion
    // builtin ticks up to 120Hz instead of the active-displays default.
    let sem = unsafe { dispatch_semaphore_create(0) };
    let display = display_for(origin);
    let mut link: CVDisplayLinkRef = std::ptr::null_mut();
    unsafe {
        CVDisplayLinkCreateWithCGDisplay(display, &mut link);
        CVDisplayLinkSetOutputCallback(link, display_link_callback, sem as *mut c_void);
        let p = CVDisplayLinkGetNominalOutputVideoRefreshPeriod(link);
        let hz = if p.time_value != 0 {
            p.time_scale as f64 / p.time_value as f64
        } else {
            0.0
        };
        println!("display {display}: link nominal refresh = {hz:.1} Hz");
        CVDisplayLinkStart(link);
    }

    // 4 + forward animation: move the real window to its destination (occluded
    // by the backdrop), then animate the proxy origin -> dest.
    move_real_window(&target.elem, dest);
    run_spring(&proxy, &live, origin, dest, expected_px(dest), sem);

    // Pause for a second to show the window at its destination.
    backdrop.hide();
    proxy.hide();
    pump_runloop(1.0);
    backdrop.show();
    proxy.show();
    proxy.order_above(&backdrop);

    // QoL: animate back. Move the real window home, then animate dest -> origin.
    move_real_window(&target.elem, origin);
    run_spring(&proxy, &live, dest, origin, expected_px(origin), sem);

    // 6: stop the capture thread and link, then tear the proxy windows down.
    stop.store(true, Ordering::Relaxed);
    let _ = capture_thread.join();
    unsafe {
        CVDisplayLinkStop(link);
        CVDisplayLinkRelease(link);
        dispatch_release(sem);
    }
    drop(proxy);
    drop(backdrop);
    unsafe { SLSReleaseConnection(cid) };
    println!("animation complete");
}

/// Move the real window via AX (size, position, size again to dodge macOS
/// visible-area clamping). Must run on the main thread for an in-process window.
fn move_real_window(elem: &AXUIElement, frame: CGRect) {
    let move_it = {
        struct MakeItSend(AXUIElement);
        let elem = MakeItSend(elem.clone());
        unsafe impl Send for MakeItSend {}
        move || {
            let elem = elem; // capture MakeItSend
            let elem = elem.0;
            let sz = cg::CGSize {
                width: frame.size.width,
                height: frame.size.height,
            };
            let pos = cg::CGPoint {
                x: frame.origin.x,
                y: frame.origin.y,
            };
            _ = elem.set_size(sz);
            _ = elem.set_position(pos);
            _ = elem.set_size(sz);
        }
    };
    if let Ok(pid) = elem.process_id()
        && pid == process::id() as pid_t
    {
        move_it();
    } else {
        std::thread::spawn(move_it);
    }
}

/// Drive the proxy from `from` to `to` with a spring, one update per vsync tick
/// (the semaphore is signalled by the display-link callback). Each frame the
/// proxy is redrawn from the latest frame in `live`, so the animating window
/// shows live content (e.g. text typed in another app). `expected_px` is the
/// pixel size a settled capture of the (already relocated) real window should
/// have; captures of a different size are mid-resize and rendered partly blank,
/// so we skip them and keep showing the last good bitmap.
fn run_spring(
    proxy: &ProxyWindow,
    live: &Mutex<Option<SendImage>>,
    from: CGRect,
    to: CGRect,
    expected_px: (usize, usize),
    sem: DispatchSemaphore,
) {
    // Drain vsync ticks that piled up since the last run (the display link keeps
    // firing during the pause between animations). Without this the semaphore
    // starts with a backlog, the first frames don't block on vsync, and the run
    // burns through them at CPU speed — inflating the reported fps and skipping
    // the vsync pacing until the backlog drains.
    while unsafe { dispatch_semaphore_wait(sem, DISPATCH_TIME_NOW) } == 0 {}

    let start = Instant::now();
    let spring = SpringAnimation::new(0.0, 1.0, 0.0, slow_response(), 1.0, start);
    let mut frames: u32 = 0;
    loop {
        // Wait for the next vsync tick (cap the wait so we never hang if the
        // link stops delivering).
        let timeout = unsafe { dispatch_time(DISPATCH_TIME_NOW, 100_000_000) };
        unsafe { dispatch_semaphore_wait(sem, timeout) };

        let now = Instant::now();
        let s = spring.value_at(now);
        let v = spring.velocity_at(now);
        frames += 1;

        // Swap in the latest captured frame and move+resize the window to it in
        // ONE no-actions transaction, so the new bitmap and the new window size
        // are presented together. With two separate commits a vsync can land
        // between them, showing the new contents at the old size for a frame —
        // which flashes and reveals the backdrop (worst on the shrinking return).
        let cur = lerp_rect(from, to, s);
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        if let Some(img) = live.lock().unwrap().as_ref() {
            // Skip mid-resize captures: they come back a wrong size and render
            // partly blank, which flashes as a black block in the proxy.
            let (w, h) = img.px;
            if w.abs_diff(expected_px.0) <= 2 && h.abs_diff(expected_px.1) <= 2 {
                proxy.set_contents(img.img);
            }
        }
        proxy.set_frame(cur);
        CATransaction::commit();
        // Turn the run loop so AppKit/Core Animation commit this frame. A
        // zero-length run avoids the ~20ms block of a timed one, keeping us near
        // vsync.
        unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.0, false) };

        let elapsed = now.duration_since(start);
        if ((s - 1.0).abs() < 0.002 && v.abs() < 0.05) || elapsed > Duration::from_secs(8) {
            CATransaction::begin();
            CATransaction::setDisableActions(true);
            proxy.set_frame(to);
            CATransaction::commit();
            unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.0, false) };
            let secs = now.duration_since(start).as_secs_f64();
            println!(
                "ran {frames} frames in {secs:.3}s ({:.0} fps)",
                frames as f64 / secs
            );
            return;
        }
    }
}

/// Print the window-server level and sub-level the server assigned to `wid`.
/// Used to check whether our proxy/backdrop actually share the target window's
/// band, or sit in a higher one that no click could put another window above.
fn dump_level(cid: SLSConnectionID, label: &str, wid: CGWindowID) {
    let mut level: c_int = 0;
    unsafe { SLSGetWindowLevel(cid, wid, &mut level) };
    let sub_level = unsafe { SLSGetWindowSubLevel(cid, wid) };
    println!("level {label}: wid={wid:<7} level={level:<4} sublevel={sub_level}");
}

/// Print the windows around `target` in z-order (frontmost first) with owner
/// pids, so we can see whether the foreground windows are actually above the
/// target and which process owns them.
fn dump_z_order(target: WindowServerId) {
    let list = window_server::get_visible_windows_with_layer(None);
    let our_pid = std::process::id() as i32;
    println!("--- z-order (frontmost first), our pid={our_pid}, target={target:?} ---");
    for (i, w) in list.iter().enumerate() {
        let mark = if w.id == target { " <== TARGET" } else { "" };
        println!(
            "  [{i:2}] wid={:<7} pid={:<7} layer={:<3} frame={:?}{mark}",
            w.id.as_u32(),
            w.pid,
            w.layer,
            w.frame
        );
    }
}

/// The builtin display's bounds (CG global coords), if there is one.
fn builtin_display_bounds() -> Option<CGRect> {
    let mut ids = [0u32; 16];
    let mut count: u32 = 0;
    unsafe {
        CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count);
        ids.iter()
            .take(count as usize)
            .find(|&&id| CGDisplayIsBuiltin(id) != 0)
            .map(|&id| CGDisplayBounds(id))
    }
}

/// The *visible* frame of `display` (excludes the menu bar and Dock), in CG
/// global coords (top-left origin). This is the region the real window can
/// actually be moved into via AX — macOS clamps an AX window to stay below the
/// menu bar. Returns `None` if we can't match an `NSScreen` to the display.
fn visible_frame(display: CGDirectDisplayID) -> Option<CGRect> {
    // `NSScreen` frames are in AppKit coords (bottom-left origin, y up), so we
    // flip the y axis through the primary display's height to reach CG coords.
    let mtm = MainThreadMarker::new()?;
    let db = unsafe { CGDisplayBounds(display) };
    let primary_h = unsafe { CGDisplayBounds(CGMainDisplayID()) }.size.height;
    let to_cg = |r: NSRect| {
        CGRect::new(
            CGPoint::new(r.origin.x, primary_h - (r.origin.y + r.size.height)),
            CGSize::new(r.size.width, r.size.height),
        )
    };
    NSScreen::screens(mtm).iter().find_map(|s| {
        let full = to_cg(s.frame());
        let same = (full.origin.x - db.origin.x).abs() < 1.0
            && (full.origin.y - db.origin.y).abs() < 1.0;
        same.then(|| to_cg(s.visibleFrame()))
    })
}

/// The display whose bounds contain the center of `rect`, else the main display.
fn display_for(rect: CGRect) -> CGDirectDisplayID {
    let center = CGPoint::new(
        rect.origin.x + rect.size.width / 2.0,
        rect.origin.y + rect.size.height / 2.0,
    );
    let mut id: CGDirectDisplayID = 0;
    let mut count: u32 = 0;
    unsafe {
        CGGetDisplaysWithPoint(center, 1, &mut id, &mut count);
        if count == 0 { CGMainDisplayID() } else { id }
    }
}

fn slow_response() -> f64 {
    std::env::var("GLIDE_RESPONSE").ok().and_then(|s| s.parse().ok()).unwrap_or(0.5)
}

/// Pick a destination frame: shift down-right and grow, so the animation
/// exercises both translation and stretch.
fn destination_for(origin: CGRect) -> CGRect {
    let dest = CGRect::new(
        CGPoint::new(origin.origin.x + 460.0, origin.origin.y + 280.0),
        CGSize::new(origin.size.width * 1.5, origin.size.height * 1.5),
    );
    // Keep the destination within the visible frame (below the menu bar, clear
    // of the Dock). Off-screen area would not be captured.
    let display = display_for(origin);
    let bounds = visible_frame(display).unwrap_or_else(|| unsafe { CGDisplayBounds(display) });
    clamp_to_rect(dest, bounds)
}

fn ensure_screen_recording() -> bool {
    let granted = unsafe { CGPreflightScreenCaptureAccess() };
    if granted {
        return true;
    }
    eprintln!("Screen Recording permission is required to capture windows.");
    eprintln!("Requesting it now; grant it in System Settings > Privacy & Security");
    eprintln!("> Screen Recording, then re-run this example.");
    unsafe { CGRequestScreenCaptureAccess() };
    false
}

// ---------------------------------------------------------------------------
// Entry points: spawn our own window, or target an existing one via --wid.
// ---------------------------------------------------------------------------

fn parse_wid() -> Option<WindowServerId> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--wid" {
            let v = args.next()?;
            return v.parse::<u32>().ok().map(WindowServerId::new);
        }
    }
    None
}

fn main() {
    if !ensure_screen_recording() {
        std::process::exit(1);
    }

    if let Some(wsid) = parse_wid() {
        // Existing-window path: no NSApplication needed, the owner renders it.
        let Some(target) = resolve_existing(wsid) else {
            eprintln!("could not resolve an AX window for {wsid:?}");
            std::process::exit(1);
        };
        let mut origin = CGRect::default();
        let mut cid: SLSConnectionID = 0;
        unsafe {
            SLSNewConnection(0, &mut cid);
            SLSGetWindowBounds(cid, wsid.as_u32(), &mut origin);
            SLSReleaseConnection(cid);
        }
        animate(&target, destination_for(origin));
        return;
    }

    // Self-spawned path. Everything runs on the main thread: this window is
    // *in-process*, so its AX frame-set is serviced by AppKit on the calling
    // thread, and AppKit window geometry must be touched on the main thread.
    // (For an external `--wid` window the AX set goes cross-process, so that
    // path is unaffected.) We drive the run loop manually instead of
    // `app.run()` so we can render, then animate inline, then exit.
    let mtm = MainThreadMarker::new().expect("must run on main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    app.finishLaunching();

    let fg_test = std::env::var("GLIDE_FG_TEST").is_ok();

    let wsid = spawn_window(
        mtm,
        NSPoint { x: 200.0, y: 400.0 },
        "glide",
        NSColor::systemTealColor(),
    );
    println!("spawned window {wsid:?}");

    // Test the foreground handling: spawn a second window *in front of* the
    // target, over the destination, so the animating proxy must pass behind it.
    if fg_test {
        spawn_window(
            mtm,
            NSPoint { x: 640.0, y: 120.0 },
            "front",
            NSColor::systemPinkColor(),
        );
    }

    // Let AppKit lay out and the window server composite the window. Retry the
    // AX lookup briefly: the window may not be in the AX tree immediately.
    let pid = std::process::id() as i32;
    let mut elem = None;
    for _ in 0..40 {
        pump_runloop(0.05);
        elem = AXUIElement::application(pid).windows().ok().and_then(|ws| {
            ws.iter()
                .find(|w| WindowServerId::try_from(&**w).ok() == Some(wsid))
                .map(|w| w.clone())
        });
        if elem.is_some() {
            break;
        }
    }
    let target = Target {
        wsid,
        elem: elem.expect("AX window not found"),
    };

    // Move the window onto the builtin display so the 120Hz path is exercised
    // (ProMotion). Skip under fg_test so both windows stay co-located.
    if let Some(b) = builtin_display_bounds().filter(|_| !fg_test) {
        move_real_window(
            &target.elem,
            CGRect::new(
                CGPoint::new(b.origin.x + 200.0, b.origin.y + 200.0),
                CGSize::new(360.0, 292.0),
            ),
        );
        pump_runloop(0.2);
    }

    let mut origin = CGRect::default();
    let mut cid: SLSConnectionID = 0;
    unsafe {
        SLSNewConnection(0, &mut cid);
        SLSGetWindowBounds(cid, wsid.as_u32(), &mut origin);
        SLSReleaseConnection(cid);
    }
    animate(&target, destination_for(origin));
    pump_runloop(0.6);
}

fn spawn_window(
    mtm: MainThreadMarker,
    origin: NSPoint,
    text: &str,
    color: Retained<NSColor>,
) -> WindowServerId {
    let frame = NSRect {
        origin,
        size: objc2_foundation::NSSize { width: 360.0, height: 260.0 },
    };
    let style =
        NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Resizable;
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setTitle(&NSString::from_str("Proxy Animation Demo"));
    window.setBackgroundColor(Some(&color));

    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setFrame(NSRect {
        origin: NSPoint { x: 0.0, y: 90.0 },
        size: objc2_foundation::NSSize { width: 360.0, height: 80.0 },
    });
    label.setAlignment(NSTextAlignment::Center);
    label.setBezeled(false);
    label.setDrawsBackground(false);
    label.setFont(Some(&*NSFont::systemFontOfSize(64.0)));
    if let Some(content) = window.contentView() {
        content.addSubview(&label);
    }

    window.makeKeyAndOrderFront(None);
    unsafe { window.setReleasedWhenClosed(false) };
    WindowServerId::new(window.windowNumber().try_into().expect("window number too large"))
}
