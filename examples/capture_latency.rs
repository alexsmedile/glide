// Copyright The Glide Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Capture latency experiment: time-to-first-frame for different capture APIs
//! and prewarming strategies. The proxy-window animation must start within
//! ~100ms; a cold `SCStream` takes ~400ms to deliver its first frame, so this
//! harness measures the alternatives.
//!
//! Each trial simulates a "steady state" idle gap (so warm streams have been
//! running a while), then *triggers* and measures the latency until a usable
//! frame is in our buffer. Run a batch (`--trials N --idle MS`) or step through
//! interactively (`--interactive`, press Enter per trial). Reports min / median
//! / max per strategy.
//!
//! Strategies (capture of a single window, via `--wid`):
//!   - legacy-sls     `SLSHWCaptureWindowList` (synchronous; fast, but triggers
//!                    the macOS private-window-picker alert — baseline only).
//!   - sck-shot       `SCScreenshotManager` one-shot (async, no persistent stream).
//!   - sck-cold       `SCStream` started on the trigger, filter cached; wait for
//!                    its first `Complete` frame.
//!   - sck-cold-fetch like sck-cold but also re-runs `SCShareableContent` first
//!                    (the realistic per-animation cost if nothing is cached).
//!   - sck-warm       `SCStream` kept running through the idle gap; on the trigger
//!                    just read the latest frame (prewarmed). Reports the frame's
//!                    staleness and the wait for a fresh post-trigger frame too.
//!   - sck-warm-1fps  prewarmed but throttled to 1 fps (cheaper to keep alive) —
//!                    shows the staleness/latency cost of throttling a warm stream.
//!
//! Run:
//!   cargo run --example capture_latency -- --wid 12345
//!   cargo run --example capture_latency -- --wid 12345 --interactive
//!   cargo run --example capture_latency -- --wid 12345 --trials 20 --idle 750
//!   cargo run --example capture_latency -- --wid 12345 --only sck-warm,sck-shot

use std::ffi::{c_int, c_void};
use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_core_foundation::{CFRetained, CGRect};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::{CVImageBuffer, CVPixelBufferGetIOSurface};
use objc2_foundation::{MainThreadMarker, NSError, NSObject, NSObjectProtocol, NSString};
use objc2_io_surface::IOSurfaceRef;
use objc2_screen_capture_kit::{
    SCContentFilter, SCFrameStatus, SCScreenshotManager, SCShareableContent, SCStream,
    SCStreamConfiguration, SCStreamFrameInfoStatus, SCStreamOutput, SCStreamOutputType, SCWindow,
};

// ---------------------------------------------------------------------------
// Minimal FFI: the legacy synchronous window capture (baseline) + a screen-
// recording permission check, and a couple of CF helpers.
// ---------------------------------------------------------------------------

type SLSConnectionID = c_int;
type CGWindowID = u32;
type CFArrayRef = *const c_void;
type CFTypeRef = *const c_void;

// SLSHWCaptureWindowList flags used by yabai (and our old proxy capture).
const SLS_CAPTURE_FLAGS: u32 = (1 << 11) | (1 << 8);

#[link(name = "SkyLight", kind = "framework")]
unsafe extern "C" {
    fn SLSNewConnection(zero: c_int, cid: *mut SLSConnectionID) -> i32;
    fn SLSReleaseConnection(cid: SLSConnectionID) -> i32;
    fn SLSHWCaptureWindowList(
        cid: SLSConnectionID,
        window_list: *const CGWindowID,
        window_count: c_int,
        options: u32,
    ) -> CFArrayRef;
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: CFTypeRef);
    fn CFArrayGetCount(array: CFArrayRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CFArrayRef, idx: isize) -> *const c_void;
}

// libdispatch semaphore for blocking on async SCK completion handlers.
type DispatchSemaphore = *mut c_void;
unsafe extern "C" {
    fn dispatch_semaphore_create(value: isize) -> DispatchSemaphore;
    fn dispatch_semaphore_signal(sem: DispatchSemaphore) -> isize;
    fn dispatch_semaphore_wait(sem: DispatchSemaphore, timeout: u64) -> isize;
    fn dispatch_release(obj: *mut c_void);
}
const DISPATCH_TIME_FOREVER: u64 = u64::MAX;

/// One captured frame plus when it landed in our buffer.
struct Frame {
    _buffer: CFRetained<CVImageBuffer>,
    _surface: CFRetained<IOSurfaceRef>,
    arrived: Instant,
}

type Slot = Arc<Mutex<Option<Frame>>>;

static SCK_FRAMES: AtomicU64 = AtomicU64::new(0);

/// Only `Complete` frames carry real pixels (idle/blank are timing frames).
fn frame_status(sbuf: &CMSampleBuffer) -> SCFrameStatus {
    let Some(attachments) = (unsafe { sbuf.sample_attachments_array(false) }) else {
        return SCFrameStatus::Complete;
    };
    let array = std::ptr::from_ref(&*attachments).cast::<c_void>();
    let dict = unsafe { CFArrayGetValueAtIndex(array, 0) };
    if dict.is_null() {
        return SCFrameStatus::Complete;
    }
    let key_ns: &NSString = unsafe { SCStreamFrameInfoStatus };
    let key = std::ptr::from_ref(key_ns).cast::<c_void>();
    let value = unsafe { CFDictionaryGetValue(dict, key) };
    if value.is_null() {
        return SCFrameStatus::Complete;
    }
    let mut status: isize = SCFrameStatus::Complete.0;
    unsafe {
        CFNumberGetValue(value, 15, std::ptr::from_mut(&mut status).cast::<c_void>());
    }
    SCFrameStatus(status)
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
    fn CFNumberGetValue(number: *const c_void, the_type: isize, value: *mut c_void) -> bool;
}

define_class! {
    #[unsafe(super(NSObject))]
    #[ivars = Slot]
    struct StreamOutput;

    unsafe impl NSObjectProtocol for StreamOutput {}

    unsafe impl SCStreamOutput for StreamOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output(&self, _s: &SCStream, sbuf: &CMSampleBuffer, kind: SCStreamOutputType) {
            if kind.0 != SCStreamOutputType::Screen.0 {
                return;
            }
            if frame_status(sbuf).0 != SCFrameStatus::Complete.0 {
                return;
            }
            let Some(image) = (unsafe { sbuf.image_buffer() }) else {
                return;
            };
            if let Some(surface) = CVPixelBufferGetIOSurface(Some(&image)) {
                *self.ivars().lock().unwrap() = Some(Frame {
                    _buffer: image,
                    _surface: surface,
                    arrived: Instant::now(),
                });
                SCK_FRAMES.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

impl StreamOutput {
    fn new(slot: Slot) -> Retained<Self> {
        let this = Self::alloc().set_ivars(slot);
        unsafe { msg_send![super(this), init] }
    }
}

/// Block on `SCShareableContent` (async API) and return it.
fn shareable_content() -> Option<Retained<SCShareableContent>> {
    let result: Arc<Mutex<Option<Retained<SCShareableContent>>>> = Arc::new(Mutex::new(None));
    let sem = unsafe { dispatch_semaphore_create(0) };
    let handler = {
        let result = result.clone();
        RcBlock::new(move |c: *mut SCShareableContent, _e: *mut NSError| {
            if !c.is_null() {
                *result.lock().unwrap() = unsafe { Retained::retain(c) };
            }
            unsafe { dispatch_semaphore_signal(sem) };
        })
    };
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };
    unsafe { dispatch_semaphore_wait(sem, DISPATCH_TIME_FOREVER) };
    unsafe { dispatch_release(sem) };
    let r = result.lock().unwrap().take();
    r
}

fn find_scwindow(content: &SCShareableContent, wsid: u32) -> Option<Retained<SCWindow>> {
    let windows = unsafe { content.windows() };
    (0..windows.count())
        .map(|i| windows.objectAtIndex(i))
        .find(|w| unsafe { w.windowID() } == wsid)
}

fn window_filter(window: &SCWindow) -> Retained<SCContentFilter> {
    unsafe { SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), window) }
}

fn config(
    px_w: usize,
    px_h: usize,
    min_interval: Option<CMTime>,
) -> Retained<SCStreamConfiguration> {
    let config = unsafe { SCStreamConfiguration::new() };
    unsafe {
        config.setWidth(px_w);
        config.setHeight(px_h);
        config.setShowsCursor(false);
        config.setQueueDepth(8);
        if let Some(t) = min_interval {
            config.setMinimumFrameInterval(t);
        }
    }
    config
}

/// A running `SCStream` feeding a slot; dropping it stops the capture.
struct RunningStream {
    stream: Retained<SCStream>,
    _output: Retained<StreamOutput>,
    slot: Slot,
}

impl Drop for RunningStream {
    fn drop(&mut self) {
        unsafe { self.stream.stopCaptureWithCompletionHandler(None) };
    }
}

fn start_stream(filter: &SCContentFilter, cfg: &SCStreamConfiguration) -> RunningStream {
    let slot: Slot = Arc::new(Mutex::new(None));
    let output = StreamOutput::new(slot.clone());
    let stream = unsafe {
        SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), filter, cfg, None)
    };
    let proto = ProtocolObject::from_ref(&*output);
    let _ = unsafe {
        stream.addStreamOutput_type_sampleHandlerQueue_error(
            proto,
            SCStreamOutputType::Screen,
            None,
        )
    };
    unsafe { stream.startCaptureWithCompletionHandler(None) };
    RunningStream { stream, _output: output, slot }
}

/// Spin (sleeping briefly) until `slot` holds a frame that arrived at/after `since`,
/// or `timeout` elapses. Returns the arrival instant if one came.
fn wait_frame_after(slot: &Slot, since: Instant, timeout: Duration) -> Option<Instant> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(f) = slot.lock().unwrap().as_ref() {
            if f.arrived >= since {
                return Some(f.arrived);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_micros(200));
    }
}

/// Legacy synchronous capture (baseline). Returns whether a frame came back.
fn legacy_capture(cid: SLSConnectionID, wid: CGWindowID) -> bool {
    unsafe {
        let array = SLSHWCaptureWindowList(cid, &wid, 1, SLS_CAPTURE_FLAGS);
        if array.is_null() {
            return false;
        }
        let ok = CFArrayGetCount(array) >= 1;
        CFRelease(array);
        ok
    }
}

/// One-shot `SCScreenshotManager` capture; blocks until the completion handler
/// fires. Returns whether a sample buffer came back.
fn screenshot(filter: &SCContentFilter, cfg: &SCStreamConfiguration) -> bool {
    let got = Arc::new(Mutex::new(false));
    let sem = unsafe { dispatch_semaphore_create(0) };
    let handler = {
        let got = got.clone();
        RcBlock::new(move |sbuf: *mut CMSampleBuffer, _e: *mut NSError| {
            *got.lock().unwrap() = !sbuf.is_null();
            unsafe { dispatch_semaphore_signal(sem) };
        })
    };
    unsafe {
        SCScreenshotManager::captureSampleBufferWithFilter_configuration_completionHandler(
            filter,
            cfg,
            Some(&handler),
        );
        dispatch_semaphore_wait(sem, DISPATCH_TIME_FOREVER);
        dispatch_release(sem);
    }
    let r = *got.lock().unwrap();
    r
}

#[derive(Clone, Copy, PartialEq)]
enum Strategy {
    LegacySls,
    SckShot,
    SckCold,
    SckColdFetch,
    SckWarm,
    SckWarm1Fps,
}

impl Strategy {
    fn name(self) -> &'static str {
        match self {
            Strategy::LegacySls => "legacy-sls",
            Strategy::SckShot => "sck-shot",
            Strategy::SckCold => "sck-cold",
            Strategy::SckColdFetch => "sck-cold-fetch",
            Strategy::SckWarm => "sck-warm",
            Strategy::SckWarm1Fps => "sck-warm-1fps",
        }
    }
    fn all() -> [Strategy; 6] {
        [
            Strategy::LegacySls,
            Strategy::SckShot,
            Strategy::SckCold,
            Strategy::SckColdFetch,
            Strategy::SckWarm,
            Strategy::SckWarm1Fps,
        ]
    }
}

struct Ctx {
    wid: u32,
    cid: SLSConnectionID,
    filter: Retained<SCContentFilter>,
    px: (usize, usize),
}

/// Result of one trial, in milliseconds.
struct Trial {
    /// Time from trigger until a frame is available to use.
    acquire_ms: f64,
    /// For warm strategies: how stale that immediately-available frame is.
    staleness_ms: Option<f64>,
    /// For warm strategies: wait for a *fresh* post-trigger frame.
    fresh_ms: Option<f64>,
}

fn run_trial(strategy: Strategy, ctx: &Ctx, warm: Option<&RunningStream>) -> Trial {
    match strategy {
        Strategy::LegacySls => {
            let t0 = Instant::now();
            let _ = legacy_capture(ctx.cid, ctx.wid);
            Trial {
                acquire_ms: ms(t0),
                staleness_ms: None,
                fresh_ms: None,
            }
        }
        Strategy::SckShot => {
            let cfg = config(ctx.px.0, ctx.px.1, None);
            let t0 = Instant::now();
            let _ = screenshot(&ctx.filter, &cfg);
            Trial {
                acquire_ms: ms(t0),
                staleness_ms: None,
                fresh_ms: None,
            }
        }
        Strategy::SckCold => {
            let cfg = config(ctx.px.0, ctx.px.1, None);
            let t0 = Instant::now();
            let stream = start_stream(&ctx.filter, &cfg);
            let got = wait_frame_after(&stream.slot, t0, Duration::from_millis(2000));
            Trial {
                acquire_ms: got
                    .map(|a| a.duration_since(t0).as_secs_f64() * 1000.0)
                    .unwrap_or(f64::NAN),
                staleness_ms: None,
                fresh_ms: None,
            }
        }
        Strategy::SckColdFetch => {
            let cfg = config(ctx.px.0, ctx.px.1, None);
            let t0 = Instant::now();
            let stream = shareable_content()
                .and_then(|c| find_scwindow(&c, ctx.wid))
                .map(|w| start_stream(&window_filter(&w), &cfg));
            let acquire = match &stream {
                Some(s) => wait_frame_after(&s.slot, t0, Duration::from_millis(2000))
                    .map(|a| a.duration_since(t0).as_secs_f64() * 1000.0)
                    .unwrap_or(f64::NAN),
                None => f64::NAN,
            };
            Trial {
                acquire_ms: acquire,
                staleness_ms: None,
                fresh_ms: None,
            }
        }
        Strategy::SckWarm | Strategy::SckWarm1Fps => {
            let warm = warm.expect("warm stream");
            let t0 = Instant::now();
            // Latest already-available frame (the prewarming win): ~0ms.
            let staleness = warm
                .slot
                .lock()
                .unwrap()
                .as_ref()
                .map(|f| t0.duration_since(f.arrived).as_secs_f64() * 1000.0);
            let acquire = ms(t0);
            // Also: wait for a fresh frame captured after the trigger.
            let fresh = wait_frame_after(&warm.slot, t0, Duration::from_millis(2000))
                .map(|a| a.duration_since(t0).as_secs_f64() * 1000.0);
            Trial {
                acquire_ms: acquire,
                staleness_ms: staleness,
                fresh_ms: fresh,
            }
        }
    }
}

fn ms(t0: Instant) -> f64 {
    t0.elapsed().as_secs_f64() * 1000.0
}

fn stats(label: &str, mut v: Vec<f64>) {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        println!("    {label}: (no data)");
        return;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let min = v[0];
    let max = v[v.len() - 1];
    let median = v[v.len() / 2];
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    println!(
        "    {label}: min={min:.1} median={median:.1} mean={mean:.1} max={max:.1} (n={})",
        v.len()
    );
}

fn parse_arg(name: &str) -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == name {
            return args.next();
        }
    }
    None
}

fn main() {
    if !unsafe { CGPreflightScreenCaptureAccess() } {
        eprintln!("Screen Recording permission required; requesting…");
        unsafe { CGRequestScreenCaptureAccess() };
        std::process::exit(1);
    }
    let mtm = MainThreadMarker::new().expect("main thread");
    NSApplication::sharedApplication(mtm)
        .setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let Some(wid) = parse_arg("--wid").and_then(|s| s.parse::<u32>().ok()) else {
        eprintln!(
            "usage: capture_latency --wid <id> [--trials N] [--idle MS] [--interactive] [--only a,b]"
        );
        std::process::exit(1);
    };
    let trials: usize = parse_arg("--trials").and_then(|s| s.parse().ok()).unwrap_or(10);
    let idle_ms: u64 = parse_arg("--idle").and_then(|s| s.parse().ok()).unwrap_or(1000);
    let interactive = std::env::args().any(|a| a == "--interactive");
    let only: Option<Vec<String>> =
        parse_arg("--only").map(|s| s.split(',').map(|x| x.trim().to_string()).collect());

    let mut cid: SLSConnectionID = 0;
    unsafe { SLSNewConnection(0, &mut cid) };

    // Resolve the target window via SCK once, to build the reusable filter.
    let Some(content) = shareable_content() else {
        eprintln!("failed to fetch shareable content");
        std::process::exit(1);
    };
    let Some(scwindow) = find_scwindow(&content, wid) else {
        eprintln!("window {wid} not found in shareable content");
        std::process::exit(1);
    };
    let frame: CGRect = unsafe { scwindow.frame() };
    let scale = 2.0;
    let px = (
        (frame.size.width * scale) as usize,
        (frame.size.height * scale) as usize,
    );
    let ctx = Ctx {
        wid,
        cid,
        filter: window_filter(&scwindow),
        px,
    };

    println!(
        "target wid={wid} size={}x{}pt -> {}x{}px; trials={trials} idle={idle_ms}ms interactive={interactive}",
        frame.size.width as i64, frame.size.height as i64, px.0, px.1
    );

    let selected: Vec<Strategy> = Strategy::all()
        .into_iter()
        .filter(|s| only.as_ref().map(|o| o.iter().any(|n| n == s.name())).unwrap_or(true))
        .collect();

    for strategy in selected {
        println!("\n== {} ==", strategy.name());
        // Prewarm where the strategy calls for it.
        let warm = match strategy {
            Strategy::SckWarm => Some(start_stream(&ctx.filter, &config(px.0, px.1, None))),
            Strategy::SckWarm1Fps => {
                let interval = unsafe { CMTime::new(1, 1) };
                Some(start_stream(&ctx.filter, &config(px.0, px.1, Some(interval))))
            }
            _ => None,
        };
        // Let a warm stream reach steady state before the first trial.
        if warm.is_some() {
            std::thread::sleep(Duration::from_millis(idle_ms.max(500)));
        }

        let mut acquire = Vec::new();
        let mut staleness = Vec::new();
        let mut fresh = Vec::new();
        for i in 0..trials {
            // Steady state: idle (or wait for Enter) before the trigger.
            if interactive {
                print!(
                    "  [{}] press Enter for trial {}/{trials}… ",
                    strategy.name(),
                    i + 1
                );
                std::io::stdout().flush().ok();
                let mut s = String::new();
                std::io::stdin().read_line(&mut s).ok();
            } else {
                std::thread::sleep(Duration::from_millis(idle_ms));
            }
            let t = run_trial(strategy, &ctx, warm.as_ref());
            acquire.push(t.acquire_ms);
            if let Some(s) = t.staleness_ms {
                staleness.push(s);
            }
            if let Some(f) = t.fresh_ms {
                fresh.push(f);
            }
            let extra = match (t.staleness_ms, t.fresh_ms) {
                (Some(s), Some(f)) => format!("  staleness={s:.1}ms fresh={f:.1}ms"),
                _ => String::new(),
            };
            println!("    trial {}: acquire={:.1}ms{extra}", i + 1, t.acquire_ms);
        }
        stats("acquire", acquire);
        if !staleness.is_empty() {
            stats("staleness", staleness);
            stats("fresh-frame", fresh);
        }
        drop(warm);
    }

    unsafe { SLSReleaseConnection(cid) };
    println!("\ndone");
}
