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
//!      (static, opaque) and the proxy (animated) above it. To keep windows
//!      that sit in front of the target visible, order the proxy stack *below*
//!      them (`SLSOrderWindow` relative to the real window above the target), so
//!      they stay live and on top. (`GLIDE_FG_SNAPSHOT` instead overlays a
//!      frozen snapshot of them — the original approach.)
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use accessibility::{AXUIElement, AXUIElementAttributes};
use core_graphics_types::geometry as cg;
use glide_wm::model::spring::SpringAnimation;
use glide_wm::sys::window_server::{self, WindowServerId};
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSFont,
    NSTextAlignment, NSTextField, NSWindow, NSWindowStyleMask,
};
use objc2_core_foundation::{CGAffineTransform, CGPoint, CGRect, CGSize};
use objc2_foundation::{NSPoint, NSRect, NSString};

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
type CGContextRef = *mut c_void;
type CGWindowImageOption = u32;
type CGWindowListOption = u32;

type CVDisplayLinkRef = *mut c_void;
type CVReturn = i32;
type CVOptionFlags = u64;

const KCG_WINDOW_LIST_OPTION_ON_SCREEN_ABOVE_WINDOW: CGWindowListOption = 1 << 1;
const KCG_WINDOW_LIST_OPTION_ON_SCREEN_BELOW_WINDOW: CGWindowListOption = 1 << 2;
const KCG_WINDOW_IMAGE_DEFAULT: CGWindowImageOption = 0;
const KCV_RETURN_SUCCESS: CVReturn = 0;

// Tag bit used by yabai when creating proxy windows.
const PROXY_WINDOW_TAG: u64 = 1 << 46;
// SLSHWCaptureWindowList flags used by yabai.
const SLS_CAPTURE_FLAGS: u32 = (1 << 11) | (1 << 8);

#[link(name = "SkyLight", kind = "framework")]
unsafe extern "C" {
    fn SLSNewConnection(zero: c_int, cid: *mut SLSConnectionID) -> i32;
    fn SLSReleaseConnection(cid: SLSConnectionID) -> i32;

    fn SLSGetWindowBounds(cid: SLSConnectionID, wid: CGWindowID, frame: *mut CGRect) -> i32;
    fn SLSGetWindowLevel(cid: SLSConnectionID, wid: CGWindowID, level: *mut c_int) -> i32;

    fn SLSNewWindowWithOpaqueShapeAndContext(
        cid: SLSConnectionID,
        window_type: c_int,
        region: CFTypeRef,
        opaque_shape: CFTypeRef,
        options: c_int,
        tags: *const u64,
        x: f32,
        y: f32,
        tag_size: c_int,
        wid: *mut CGWindowID,
        context: *mut c_void,
    ) -> i32;
    fn SLSReleaseWindow(cid: SLSConnectionID, wid: CGWindowID) -> i32;
    fn SLWindowContextCreate(
        cid: SLSConnectionID,
        wid: CGWindowID,
        options: CFTypeRef,
    ) -> CGContextRef;

    fn SLSSetWindowResolution(cid: SLSConnectionID, wid: CGWindowID, resolution: f64) -> i32;
    fn SLSSetWindowOpacity(cid: SLSConnectionID, wid: CGWindowID, opaque: bool) -> i32;
    fn SLSSetWindowAlpha(cid: SLSConnectionID, wid: CGWindowID, alpha: f32) -> i32;
    fn SLSSetWindowLevel(cid: SLSConnectionID, wid: CGWindowID, level: c_int) -> i32;
    fn SLSWindowSetShadowProperties(wid: CGWindowID, options: CFTypeRef) -> i32;
    fn SLSOrderWindow(cid: SLSConnectionID, wid: CGWindowID, mode: c_int, rel: CGWindowID) -> i32;

    fn SLSDisableUpdate(cid: SLSConnectionID) -> i32;
    fn SLSReenableUpdate(cid: SLSConnectionID) -> i32;

    fn SLSHWCaptureWindowList(
        cid: SLSConnectionID,
        window_list: *const CGWindowID,
        window_count: c_int,
        options: u32,
    ) -> CFArrayRef;

    // Transactions: a synchronous commit flushes to the window server without
    // needing a run-loop turn, which is what lets us hit full refresh rate.
    fn SLSTransactionCreate(cid: SLSConnectionID) -> CFTypeRef;
    fn SLSTransactionCommit(transaction: CFTypeRef, synchronous: c_int) -> i32;
    fn SLSTransactionSetWindowTransform(
        transaction: CFTypeRef,
        wid: CGWindowID,
        unknown: c_int,
        unknown2: c_int,
        transform: CGAffineTransform,
    ) -> i32;

    fn CGRegionCreateEmptyRegion() -> CFTypeRef;
    fn CGSNewRegionWithRect(rect: *const CGRect, region: *mut CFTypeRef) -> i32;
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGWindowListCreateImage(
        bounds: CGRect,
        list_option: CGWindowListOption,
        window_id: CGWindowID,
        image_option: CGWindowImageOption,
    ) -> CGImageRef;
    fn CGContextClearRect(ctx: CGContextRef, rect: CGRect);
    fn CGContextDrawImage(ctx: CGContextRef, rect: CGRect, image: CGImageRef);
    fn CGContextFlush(ctx: CGContextRef);
    fn CGContextRelease(ctx: CGContextRef);
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
// A server-owned window holding a static bitmap (the proxy or the backdrop).
// ---------------------------------------------------------------------------

struct ProxyWindow {
    cid: SLSConnectionID,
    id: CGWindowID,
    context: CGContextRef,
    image: CGImageRef,
    frame: CGRect,
}

impl ProxyWindow {
    /// Create a window on `cid` at `frame`, draw `image` into it, and order it
    /// in at `level`. Takes ownership of `image` (released on drop).
    fn new(
        cid: SLSConnectionID,
        frame: CGRect,
        image: CGImageRef,
        level: c_int,
        opaque: bool,
    ) -> Self {
        assert!(!image.is_null(), "cannot build a proxy from a null image");
        unsafe {
            let mut region: CFTypeRef = std::ptr::null();
            CGSNewRegionWithRect(&frame, &mut region);
            let empty = CGRegionCreateEmptyRegion();

            let tags = PROXY_WINDOW_TAG;
            let mut id: CGWindowID = 0;
            // type 2, options 13 | (1<<18), tag_size 64: the exact incantation
            // yabai uses to create a layer-backed, context-drawable window.
            SLSNewWindowWithOpaqueShapeAndContext(
                cid,
                2,
                region,
                empty,
                13 | (1 << 18),
                &tags,
                0.0,
                0.0,
                64,
                &mut id,
                std::ptr::null_mut(),
            );
            disable_shadow(id);
            SLSSetWindowResolution(cid, id, 2.0);
            SLSSetWindowOpacity(cid, id, opaque);
            SLSSetWindowAlpha(cid, id, 1.0);
            SLSSetWindowLevel(cid, id, level);

            let context = SLWindowContextCreate(cid, id, std::ptr::null());
            let local = CGRect::new(CGPoint::new(0.0, 0.0), frame.size);
            CGContextClearRect(context, local);
            CGContextDrawImage(context, local, image);
            CGContextFlush(context);

            CFRelease(region);
            CFRelease(empty);

            // Order it in above everything at its level.
            SLSOrderWindow(cid, id, 1, 0);

            ProxyWindow { cid, id, context, image, frame }
        }
    }

    /// Replace the proxy's content with a freshly captured frame. The window's
    /// position/scale come from the transform, so we only redraw the bitmap into
    /// the window-local rect. Cheap enough to do every frame for one window.
    fn redraw(&self, image: CGImageRef) {
        if image.is_null() {
            return;
        }
        let local = CGRect::new(CGPoint::new(0.0, 0.0), self.frame.size);
        unsafe {
            CGContextClearRect(self.context, local);
            CGContextDrawImage(self.context, local, image);
            CGContextFlush(self.context);
        }
    }
}

/// A `CGImageRef` that can be sent between threads and releases on drop. Used to
/// hand freshly captured frames from the capture thread to the animation loop.
struct SendImage(CGImageRef);
unsafe impl Send for SendImage {}
impl Drop for SendImage {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

impl Drop for ProxyWindow {
    fn drop(&mut self) {
        unsafe {
            SLSOrderWindow(self.cid, self.id, 0, 0);
            if !self.context.is_null() {
                CGContextRelease(self.context);
            }
            if !self.image.is_null() {
                CFRelease(self.image);
            }
            SLSReleaseWindow(self.cid, self.id);
        }
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

/// Position *and* scale the proxy to `cur`, using yabai's transform recipe.
///
/// The SLS window transform maps *screen → window* (the inverse of where the
/// content should land), so to show the captured bitmap at `cur` we translate by
/// the negated current position and scale by original/current. See
/// `window_manager.c:580` in yabai.
///
/// We apply it inside a *synchronous* SLS transaction commit. That flushes the
/// change to the window server immediately, so — unlike a bare
/// `SLSSetWindowTransform` — it does not need a run-loop turn to become visible,
/// which is what lets the loop run at full display refresh.
fn set_proxy_transform(cid: SLSConnectionID, proxy_id: CGWindowID, origin: CGRect, cur: CGRect) {
    let sx = origin.size.width / cur.size.width;
    let sy = origin.size.height / cur.size.height;
    let transform = CGAffineTransform {
        a: sx,
        b: 0.0,
        c: 0.0,
        d: sy,
        tx: -cur.origin.x * sx,
        ty: -cur.origin.y * sy,
    };
    unsafe {
        let txn = SLSTransactionCreate(cid);
        SLSTransactionSetWindowTransform(txn, proxy_id, 0, 0, transform);
        SLSTransactionCommit(txn, 1);
        CFRelease(txn);
    }
}

/// Disable the proxy window's drop shadow by zeroing its shadow density, the
/// way yabai does (`sls_window_disable_shadow`). Passing a null options dict is
/// a no-op, so we must build the dictionary explicitly.
fn disable_shadow(id: CGWindowID) {
    use core_foundation::base::TCFType;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;

    let key = CFString::from_static_string("com.apple.WindowShadowDensity");
    let value = CFNumber::from(0i32);
    let dict = CFDictionary::from_CFType_pairs(&[(key.as_CFType(), value.as_CFType())]);
    unsafe {
        SLSWindowSetShadowProperties(id, dict.as_concrete_TypeRef() as CFTypeRef);
    }
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

/// Capture everything *in front of* the window in `bounds` (window excluded).
/// The proxy is ordered above all real windows, so without this any windows that
/// legitimately sat in front of the target would be wrongly covered by the
/// animating proxy. We re-overlay this snapshot on top of the proxy to restore
/// them. Areas with nothing in front are transparent, so the proxy shows
/// through. Returns a +1-retained `CGImageRef`, or `None` if nothing is above.
fn capture_foreground(bounds: CGRect, wid: CGWindowID) -> Option<CGImageRef> {
    capture_relative_to_window(bounds, wid, KCG_WINDOW_LIST_OPTION_ON_SCREEN_ABOVE_WINDOW)
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
    // Foreground handling. By default we slide our proxy stack *below* the real
    // windows in front of the target, so they stay live and on top (the key
    // window keeps updating). GLIDE_FG_SNAPSHOT instead overlays a frozen
    // snapshot of them on top of the proxy (the original approach).
    let use_snapshot = std::env::var("GLIDE_FG_SNAPSHOT").is_ok();
    let above = window_above(target.wsid);
    println!("window above target: {above:?} (snapshot={use_snapshot})");
    let foreground_img = if use_snapshot {
        capture_foreground(backdrop_bounds, wid)
    } else {
        None
    };

    // 3: create the layers, bottom to top: backdrop (opaque, hides the real
    // target) → proxy (the animated window). Optionally a foreground snapshot.
    unsafe { SLSDisableUpdate(cid) };
    let backdrop = ProxyWindow::new(cid, backdrop_bounds, backdrop_img, level, true);
    let proxy = ProxyWindow::new(cid, origin, proxy_img, level, false);
    unsafe { SLSOrderWindow(cid, proxy.id, 1, backdrop.id) };
    if !use_snapshot && let Some(neighbor) = above {
        // Drop the whole stack just below the real foreground windows.
        unsafe {
            SLSOrderWindow(cid, backdrop.id, -1, neighbor);
            SLSOrderWindow(cid, proxy.id, 1, backdrop.id);
        }
    }
    let foreground = foreground_img.map(|img| {
        let fg = ProxyWindow::new(cid, backdrop_bounds, img, level, false);
        unsafe { SLSOrderWindow(cid, fg.id, 1, proxy.id) };
        fg
    });
    unsafe { SLSReenableUpdate(cid) };

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
            while !stop.load(Ordering::Relaxed) {
                if let Some(img) = capture_window(cap_cid, wid) {
                    *live.lock().unwrap() = Some(SendImage(img));
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
    run_spring(&proxy, &live, origin, origin, dest, sem);

    // QoL: animate back. Move the real window home, then animate dest -> origin.
    move_real_window(&target.elem, origin);
    run_spring(&proxy, &live, origin, dest, origin, sem);

    // 6: stop the capture thread and link, then tear the proxies down atomically.
    stop.store(true, Ordering::Relaxed);
    let _ = capture_thread.join();
    unsafe {
        CVDisplayLinkStop(link);
        CVDisplayLinkRelease(link);
        dispatch_release(sem);
        SLSDisableUpdate(cid);
    }
    drop(foreground);
    drop(proxy);
    drop(backdrop);
    unsafe {
        SLSReenableUpdate(cid);
        SLSReleaseConnection(cid);
    }
    println!("animation complete");
}

/// Move the real window via AX (size, position, size again to dodge macOS
/// visible-area clamping). Must run on the main thread for an in-process window.
fn move_real_window(elem: &AXUIElement, frame: CGRect) {
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

/// Drive the proxy from `from` to `to` with a spring, one update per vsync tick
/// (the semaphore is signalled by the display-link callback). `capture` is the
/// frame the proxy bitmap was captured at — the transform scales relative to it.
/// Each frame the proxy is redrawn from the latest frame in `live`, so the
/// animating window shows live content (e.g. text typed in another app).
fn run_spring(
    proxy: &ProxyWindow,
    live: &Mutex<Option<SendImage>>,
    capture: CGRect,
    from: CGRect,
    to: CGRect,
    sem: DispatchSemaphore,
) {
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

        // Swap in the latest captured frame, then position+scale it.
        if let Some(img) = live.lock().unwrap().as_ref() {
            proxy.redraw(img.0);
        }
        let cur = lerp_rect(from, to, s);
        set_proxy_transform(proxy.cid, proxy.id, capture, cur);

        let elapsed = now.duration_since(start);
        if ((s - 1.0).abs() < 0.002 && v.abs() < 0.05) || elapsed > Duration::from_secs(8) {
            set_proxy_transform(proxy.cid, proxy.id, capture, to);
            let secs = now.duration_since(start).as_secs_f64();
            println!(
                "ran {frames} frames in {secs:.3}s ({:.0} fps)",
                frames as f64 / secs
            );
            return;
        }
    }
}

/// The window-server id of the window immediately *above* `target` in z-order,
/// if any. We order our proxy stack just below it so the real foreground
/// windows (including the key window the user is typing into) stay live and on
/// top, rather than snapshotting them.
fn window_above(target: WindowServerId) -> Option<CGWindowID> {
    // get_visible_windows_with_layer returns windows frontmost-first, so the
    // entry just before the target is the one directly above it.
    let list = window_server::get_visible_windows_with_layer(None);
    let idx = list.iter().position(|w| w.id == target)?;
    idx.checked_sub(1).map(|i| list[i].id.as_u32())
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
    CGRect::new(
        CGPoint::new(origin.origin.x + 460.0, origin.origin.y + 280.0),
        CGSize::new(origin.size.width * 1.5, origin.size.height * 1.5),
    )
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
