// Copyright The Glide Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::cell::RefCell;
use std::mem;
use std::rc::{Rc, Weak};

use objc2::MainThreadMarker;
use tracing::{debug, instrument, trace, warn};

pub use crate::actor::app::pid_t;
use crate::actor::app::{self, AppInfo, AppThreadHandle, Quiet, WindowId, WindowInfo};
use crate::actor::{self, reactor, space_manager};
use crate::collections::{HashMap, HashSet};
use crate::sys::event::MouseState;
use crate::sys::screen::{NSScreenInfo, ScreenCache, SpaceId};
use crate::sys::window_server::{
    self as sys_ws, SkylightConnection, SkylightNotifier, WindowServerId, WindowsOnScreen,
    kCGSWindowIsTerminated, kCGSWindowIsVisible,
};

/// CGWindowLevel values for windows we care about. Everything else (e.g.
/// status bar items, screensavers, system overlays) is filtered out early.
const LAYER_NORMAL: i32 = 0; // kCGNormalWindowLevel
const LAYER_FLOATING: i32 = 3; // kCGFloatingWindowLevel
const LAYER_STATUS: i32 = 8; // kCGStatusWindowLevel (used by some panels)

// ---------------------------------------------------------------------------
// WindowServer – off main thread
// ---------------------------------------------------------------------------

/// Actor that takes events from app actors and adds information from the window
/// server before sending them on to the Reactor via the SpaceManager.
pub struct WindowServer {
    screen_cache: ScreenCache,
    /// Current space on each screen.
    cur_space: Vec<SpaceId>,
    /// Window server IDs currently visible on screen.
    visible_window_ids: HashSet<WindowServerId>,
    visible_windows: HashMap<WindowServerId, sys_ws::WindowServerInfo>,
    sm_tx: space_manager::Sender,
    skylight_tx: SkylightSender,
}

#[derive(Debug)]
pub enum Event {
    // Sent by the NotificationCenter actor.
    /// Screen configuration changed. Carries NSScreenInfo gathered on the main thread.
    ScreenParametersChanged(Vec<NSScreenInfo>),
    /// The active space changed.
    SpaceChanged,

    // Sent by the App actor.
    /// This is to work around a bug introduced in macOS Sequoia where
    /// kAXUIElementDestroyedNotification is not always sent correctly.
    ///
    /// See https://github.com/glide-wm/glide/issues/10.
    RegisterWindow(WindowServerId, WindowId, AppThreadHandle),
    /// A new window was created.
    WindowCreated(WindowId, WindowInfo, MouseState),
    /// The main window of an application changed.
    ApplicationMainWindowChanged(pid_t, Option<WindowId>, Quiet),
    /// A window was minimized or unminimized.
    WindowVisibilityChanged(WindowId),
    /// A window became visible.
    WindowBecameVisible(pid_t, WindowServerId),
    WindowDestroyed(WindowId),
    ApplicationLaunched {
        pid: pid_t,
        handle: AppThreadHandle,
        info: AppInfo,
        is_frontmost: bool,
        main_window: Option<WindowId>,
        visible_windows: Vec<(WindowId, WindowInfo)>,
    },
    /// Reactor event passthrough.
    ///
    /// All reactor events go through us so they reach the reactor in the
    /// correct order with respect to the other events above.
    ReactorEvent(reactor::Event),
    /// Sent by SpaceManager when it needs a fresh window list (e.g. after
    /// toggling a space or exiting expose).
    RequestSpaceRefresh,
}

pub type Sender = actor::Sender<Event>;
pub type Receiver = actor::Receiver<Event>;

impl WindowServer {
    pub fn new(sm_tx: space_manager::Sender, skylight_tx: SkylightSender) -> Self {
        Self {
            screen_cache: ScreenCache::new(),
            cur_space: Vec::new(),
            visible_window_ids: HashSet::default(),
            visible_windows: HashMap::default(),
            sm_tx,
            skylight_tx,
        }
    }

    pub async fn run(mut self, mut events_rx: Receiver) {
        while let Some((span, event)) = events_rx.recv().await {
            let _span = span.entered();
            self.on_event(event);
        }
    }

    #[instrument(skip(self))]
    fn on_event(&mut self, event: Event) {
        if let Event::ReactorEvent(..) | Event::WindowBecameVisible(..) = &event {
            trace!("event");
        } else {
            debug!("event");
        }
        match event {
            Event::RegisterWindow(wsid, wid, tx) => {
                self.skylight_tx.send(SkylightRequest::TrackWindow(wsid, wid, tx));
            }
            Event::ScreenParametersChanged(ns_screens) => {
                let Some((screens, converter)) = self.screen_cache.update_screen_config(ns_screens)
                else {
                    return;
                };
                let spaces = self.get_spaces();
                let on_screen = self.get_windows_on_screen();
                self.sm_tx.send(space_manager::Event::ScreenParametersChanged {
                    screens: screens.iter().map(|s| s.id).collect(),
                    frames: screens.iter().map(|s| s.visible_frame).collect(),
                    converter,
                    spaces,
                    scale_factors: screens.iter().map(|s| s.scale_factor).collect(),
                    on_screen,
                });
            }
            Event::SpaceChanged | Event::RequestSpaceRefresh => {
                let spaces = self.get_spaces();
                let on_screen = self.get_windows_on_screen();
                self.sm_tx.send(space_manager::Event::SpaceChanged(spaces, on_screen));
            }
            Event::WindowCreated(wid, info, mouse_state) => {
                self.send_reactor_event(reactor::Event::WindowCreated(wid, info, mouse_state));
                self.send_windows_on_screen_if_changed(Some(wid.pid));
            }
            Event::WindowDestroyed(wid) => {
                self.send_windows_on_screen_if_changed(Some(wid.pid));
                self.send_reactor_event(reactor::Event::WindowDestroyed(wid));
            }
            Event::ApplicationMainWindowChanged(pid, wid, quiet) => {
                self.send_windows_on_screen_if_changed(Some(pid));
                self.send_reactor_event(reactor::Event::ApplicationMainWindowChanged(
                    pid, wid, quiet,
                ));
            }
            Event::WindowVisibilityChanged(window_id) => {
                self.send_windows_on_screen_if_changed(Some(window_id.pid));
            }
            Event::WindowBecameVisible(pid, wsid) => {
                if self.visible_window_ids.contains(&wsid) {
                    return;
                }
                // This is a trace log above because we ignore many of these
                // events, but make it a debug log if we detected a change.
                debug!("event");
                self.send_windows_on_screen_if_changed(Some(pid));
            }
            Event::ApplicationLaunched {
                pid,
                handle,
                info,
                is_frontmost,
                main_window,
                visible_windows,
            } => {
                let on_screen = self.get_windows_on_screen();
                self.send_reactor_event(reactor::Event::WindowsOnScreenUpdated {
                    pid: Some(pid),
                    on_screen,
                });
                self.send_reactor_event(reactor::Event::ApplicationLaunched {
                    pid,
                    handle,
                    info,
                    is_frontmost,
                    main_window,
                    visible_windows,
                });
            }
            Event::ReactorEvent(event) => self.send_reactor_event(event),
        }
    }

    /// Queries the window server for visible windows and sends a
    /// `WindowsOnScreenUpdated` event if the list changed.
    fn send_windows_on_screen_if_changed(&mut self, pid: Option<pid_t>) {
        let old_windows = mem::take(&mut self.visible_windows);
        let old_ids = old_windows
            .iter()
            .filter(|(_id, w)| pid.is_none() || pid == Some(w.pid))
            .map(|(id, _w)| *id)
            .collect::<HashSet<_>>();
        let on_screen = self.get_windows_on_screen();
        let new_ids = on_screen
            .visible
            .iter()
            .filter(|id| pid.is_none() || pid == Some(id.pid))
            .flat_map(|id| id.wsid())
            .collect::<HashSet<_>>();
        debug!(?old_ids, ?new_ids);
        #[allow(irrefutable_let_patterns)]
        if let added_ids = new_ids.difference(&old_ids).copied().collect::<Vec<_>>().as_slice()
            && let &[new_id] = added_ids
            && let removed_ids =
                old_ids.difference(&new_ids).copied().collect::<Vec<_>>().as_slice()
            && let &[old_id] = removed_ids
            && let Some(new) = self.visible_windows.get(&new_id)
            && let Some(old) = old_windows.get(&old_id)
            && new.pid == old.pid
        {
            debug!("{old_id:?} => {new_id:?}");
            if let [old, new] = Self::get_windows(&[old_id, new_id]).as_slice()
                && (old.layer != new.layer || old.frame != new.frame)
            {
                // Not actually the same.
                debug!("{old:?} != {new:?}");
            } else {
                warn!("Tab detected: {old:?} => {new:?}");
                self.send_reactor_event(reactor::Event::WindowReplaced {
                    old: WindowId::with_wsid(old.pid, old_id),
                    new: WindowId::with_wsid(new.pid, new_id),
                });
                return;
            }
        }
        if new_ids != old_ids {
            self.send_reactor_event(reactor::Event::WindowsOnScreenUpdated { pid, on_screen });
        }
    }

    fn get_spaces(&mut self) -> Vec<Option<SpaceId>> {
        let spaces = self.screen_cache.get_screen_spaces();
        self.cur_space = spaces.iter().copied().flatten().collect();
        spaces
    }

    fn get_windows_on_screen(&mut self) -> WindowsOnScreen {
        let windows: Vec<_> = self
            .get_all_visible_windows()
            .into_iter()
            .filter(|w| matches!(w.layer, LAYER_NORMAL | LAYER_FLOATING | LAYER_STATUS))
            .collect();
        self.visible_windows = windows.iter().map(|w| (w.id, w.clone())).collect();
        self.visible_window_ids = windows.iter().map(|w| w.id).collect();
        WindowsOnScreen::new(windows)
    }

    #[cfg(not(test))]
    fn get_all_visible_windows(&self) -> Vec<sys_ws::WindowServerInfo> {
        // Scope the query to the current space on each screen so that windows
        // from a space we are switching away from don't leak in or out during
        // the transition.
        sys_ws::get_visible_windows_on_spaces(&self.cur_space)
    }

    #[cfg(test)]
    fn get_all_visible_windows(&self) -> Vec<sys_ws::WindowServerInfo> {
        MOCK_VISIBLE_WINDOWS.with(|w| w.borrow().clone())
    }

    /// Describes the given window server ids. Used to double-check a
    /// candidate tab swap against the current server-side geometry.
    #[cfg(not(test))]
    fn get_windows(ids: &[WindowServerId]) -> Vec<sys_ws::WindowServerInfo> {
        sys_ws::get_windows(ids)
    }

    #[cfg(test)]
    fn get_windows(ids: &[WindowServerId]) -> Vec<sys_ws::WindowServerInfo> {
        MOCK_WINDOWS
            .with(|w| w.borrow().iter().filter(|info| ids.contains(&info.id)).cloned().collect())
    }

    fn send_reactor_event(&self, event: reactor::Event) {
        self.sm_tx.send(space_manager::Event::ReactorEvent(event));
    }
}

// ---------------------------------------------------------------------------
// SkylightWatcher – main thread only
// ---------------------------------------------------------------------------

/// Watches for Skylight window-server events. Requires the main thread because
/// of `SkylightConnection`.
pub struct SkylightWatcher(Rc<RefCell<SkylightWatcherState>>);

struct SkylightWatcherState {
    connection: SkylightConnection,
    notifiers: Vec<SkylightNotifier>,
    weak_self: Weak<RefCell<Self>>,
    registered_windows: HashMap<WindowServerId, (WindowId, AppThreadHandle)>,
    ws_tx: Sender,
}

/// Commands sent from the reactor-thread `WindowServer` to the main-thread
/// `SkylightWatcher`.
#[derive(Debug)]
pub enum SkylightRequest {
    TrackWindow(WindowServerId, WindowId, AppThreadHandle),
}

pub type SkylightSender = actor::Sender<SkylightRequest>;
pub type SkylightReceiver = actor::Receiver<SkylightRequest>;

impl SkylightWatcher {
    pub fn new(mtm: MainThreadMarker, ws_tx: Sender) -> Self {
        Self(Rc::new_cyclic(
            |weak_self: &Weak<RefCell<SkylightWatcherState>>| {
                let mut state = SkylightWatcherState {
                    connection: SkylightConnection::new(mtm),
                    notifiers: vec![],
                    weak_self: weak_self.clone(),
                    registered_windows: HashMap::default(),
                    ws_tx,
                };
                state.register_callbacks();
                RefCell::new(state)
            },
        ))
    }

    pub async fn run(self, mut commands_rx: SkylightReceiver) {
        while let Some((span, command)) = commands_rx.recv().await {
            let _span = span.entered();
            let mut state = self.0.borrow_mut();
            state.on_command(command);
        }
    }
}

impl SkylightWatcherState {
    fn register_callbacks(&mut self) {
        self.register_callback(kCGSWindowIsTerminated, |this, wsid| {
            this.on_window_destroyed(wsid)
        });
        self.register_callback(kCGSWindowIsVisible, |this, wsid| {
            let Some((wid, _)) = this.registered_windows.get(&wsid) else {
                warn!("Got kCGSWindowIsVisible for unregistered window {wsid:?}");
                return;
            };
            this.ws_tx.send(Event::WindowBecameVisible(wid.pid, wsid))
        });
    }

    fn register_callback(&mut self, event: u32, callback: fn(&mut Self, WindowServerId)) {
        let weak_self = self.weak_self.clone();
        let expected_event = event;
        let notifier = self
            .connection
            .on_event(event, move |callback_event, data| {
                if callback_event != expected_event {
                    return;
                }
                let wsid = WindowServerId(u32::from_ne_bytes(
                    data.try_into().expect("data should be a CGWindowID"),
                ));
                let Some(state) = weak_self.upgrade() else {
                    warn!("could not upgrade state in callback");
                    return;
                };
                callback(&mut state.borrow_mut(), wsid);
            })
            .expect("Initializing SkylightNotifier");
        self.notifiers.push(notifier);
    }

    fn on_command(&mut self, command: SkylightRequest) {
        match command {
            SkylightRequest::TrackWindow(wsid, wid, tx) => {
                debug!("Window registered: {wsid:?}");
                self.registered_windows.insert(wsid, (wid, tx));
                if let Err(e) = self.connection.add_window(wsid) {
                    warn!("Failed to update SkylightConnection window list: {e}");
                }
            }
        }
    }

    fn on_window_destroyed(&mut self, wsid: WindowServerId) {
        debug!("Window destroyed: {wsid:?}");
        let Some((wid, tx)) = self.registered_windows.remove(&wsid) else {
            return;
        };
        self.connection.on_window_destroyed(wsid);
        // Notify the app actor so it can clean up its local state.
        _ = tx.send(app::Request::WindowDestroyed(wid));
        // Forward to WindowServer. We treat this event as authoritative so that
        // tab detection in WindowServer works when a tab is closed. Using the
        // app actor event was problematic because of lack of synchronization
        // with the window server.
        self.ws_tx.send(Event::WindowDestroyed(wid));
    }
}

#[cfg(test)]
thread_local! {
    static MOCK_VISIBLE_WINDOWS: RefCell<Vec<sys_ws::WindowServerInfo>> = RefCell::new(vec![]);
    /// Backs `WindowServer::get_windows` in tests. Defaults to empty,
    /// which models a window that is no longer present on the server.
    static MOCK_WINDOWS: RefCell<Vec<sys_ws::WindowServerInfo>> = RefCell::new(vec![]);
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};
    use test_log::test;

    use super::*;
    use crate::actor::{self, space_manager};
    use crate::sys::window_server::{WindowServerId, WindowServerInfo};

    fn wsid(id: u32) -> WindowServerId {
        WindowServerId::new(id)
    }

    fn make_window(id: u32, layer: i32) -> WindowServerInfo {
        WindowServerInfo {
            id: wsid(id),
            pid: 1,
            layer,
            frame: CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(100.0, 100.0)),
        }
    }

    fn set_mock_windows(windows: Vec<WindowServerInfo>) {
        MOCK_VISIBLE_WINDOWS.with(|w| *w.borrow_mut() = windows);
    }

    struct TestHarness {
        ws: WindowServer,
        sm_rx: space_manager::Receiver,
        #[expect(dead_code)]
        skylight_rx: SkylightReceiver,
    }

    impl TestHarness {
        fn new() -> Self {
            let (sm_tx, sm_rx) = actor::channel();
            let (skylight_tx, skylight_rx) = actor::channel();
            let ws = WindowServer::new(sm_tx, skylight_tx);
            Self { ws, sm_rx, skylight_rx }
        }

        fn on_event(&mut self, event: Event) {
            self.ws.on_event(event);
        }

        fn drain_sm(&mut self) -> Vec<space_manager::Event> {
            let mut events = vec![];
            while let Ok((_, event)) = self.sm_rx.try_recv() {
                events.push(event);
            }
            events
        }
    }

    fn find_reactor_events(sm_events: &[space_manager::Event]) -> Vec<&reactor::Event> {
        sm_events
            .iter()
            .filter_map(|e| match e {
                space_manager::Event::ReactorEvent(re) => Some(re),
                _ => None,
            })
            .collect()
    }

    fn find_windows_on_screen_updated<'a>(
        reactor_events: &'a [&'a reactor::Event],
    ) -> Vec<&'a WindowsOnScreen> {
        reactor_events
            .iter()
            .filter_map(|e| match e {
                reactor::Event::WindowsOnScreenUpdated { on_screen, .. } => Some(on_screen),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn filters_irrelevant_layers() {
        set_mock_windows(vec![
            make_window(1, LAYER_NORMAL),   // 0 – keep
            make_window(2, LAYER_FLOATING), // 3 – keep
            make_window(3, LAYER_STATUS),   // 8 – keep
            make_window(4, 25),             // screensaver – filter
            make_window(5, -1),             // desktop – filter
        ]);

        let mut h = TestHarness::new();
        h.on_event(Event::WindowVisibilityChanged(WindowId::new(1, 1)));
        let sm_events = h.drain_sm();
        let reactor_events = find_reactor_events(&sm_events);
        let updates = find_windows_on_screen_updated(&reactor_events);

        assert_eq!(updates.len(), 1);
        let visible_ids: Vec<u32> =
            updates[0].visible.iter().flat_map(|id| Some(id.wsid()?.as_u32())).collect();
        assert_eq!(visible_ids, vec![1, 2, 3]);
    }

    #[test]
    fn no_event_when_visible_windows_unchanged() {
        set_mock_windows(vec![make_window(1, LAYER_NORMAL)]);

        let mut h = TestHarness::new();
        // First call: visible_window_ids goes from [] to [1] – changed.
        h.on_event(Event::WindowVisibilityChanged(WindowId::new(1, 1)));
        let sm_events = h.drain_sm();
        let reactor_events = find_reactor_events(&sm_events);
        assert_eq!(find_windows_on_screen_updated(&reactor_events).len(), 1);

        // Second call: visible_window_ids is still [1] – no change.
        h.on_event(Event::WindowVisibilityChanged(WindowId::new(1, 1)));
        let sm_events = h.drain_sm();
        let reactor_events = find_reactor_events(&sm_events);
        assert_eq!(find_windows_on_screen_updated(&reactor_events).len(), 0);
    }

    #[test]
    fn event_sent_when_visible_windows_change() {
        set_mock_windows(vec![make_window(1, LAYER_NORMAL)]);

        let mut h = TestHarness::new();
        h.on_event(Event::WindowVisibilityChanged(WindowId::new(1, 1)));
        h.drain_sm();

        // Change the mock.
        set_mock_windows(vec![make_window(1, LAYER_NORMAL), make_window(2, LAYER_NORMAL)]);
        h.on_event(Event::WindowVisibilityChanged(WindowId::new(1, 1)));
        let sm_events = h.drain_sm();
        let reactor_events = find_reactor_events(&sm_events);
        let updates = find_windows_on_screen_updated(&reactor_events);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].visible.len(), 2);
    }

    #[test]
    fn window_created_sends_windows_on_screen_if_changed() {
        set_mock_windows(vec![make_window(1, LAYER_NORMAL)]);

        let mut h = TestHarness::new();
        let wid = WindowId::new(1, 1);
        let info = WindowInfo {
            is_standard: true,
            title: String::new().into(),
            frame: CGRect::ZERO,
            sys_id: None,
            is_resizable: true,
        };
        h.on_event(Event::WindowCreated(wid, info, MouseState::Up));
        let sm_events = h.drain_sm();
        let reactor_events = find_reactor_events(&sm_events);

        // Should have WindowCreated and WindowsOnScreenUpdated.
        assert!(reactor_events.iter().any(|e| matches!(e, reactor::Event::WindowCreated(..))));
        assert_eq!(find_windows_on_screen_updated(&reactor_events).len(), 1);
    }
}
