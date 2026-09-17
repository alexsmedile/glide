// Copyright The Glide Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::f64;
use std::ffi::{c_int, c_void};
use std::mem::MaybeUninit;
use std::num::NonZeroU64;
use std::ptr::NonNull;
use std::str::FromStr;

use bitflags::bitflags;
use objc2::rc::Retained;
use objc2::{ClassType, msg_send};
use objc2_app_kit::NSScreen;
use objc2_core_foundation::{CFArray, CFRetained, CFString, CGPoint, CGRect};
use objc2_core_graphics::{CGDisplayBounds, CGError, CGGetActiveDisplayList};
use objc2_foundation::{MainThreadMarker, NSArray, NSDictionary, NSNumber, NSString, ns_string};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub struct SpaceId(NonZeroU64);

impl SpaceId {
    #[cfg(test)]
    pub fn new(id: u64) -> SpaceId {
        SpaceId(NonZeroU64::new(id).unwrap())
    }

    pub fn get(&self) -> u64 {
        self.0.get()
    }
}

/// Calculates the screen and space configuration.
pub struct ScreenCache<S: System = Actual> {
    system: S,
    uuids: Vec<CFRetained<CFString>>,
}

impl ScreenCache<Actual> {
    pub fn new() -> Self {
        Self::new_with(Actual)
    }
}

impl<S: System> ScreenCache<S> {
    fn new_with(system: S) -> ScreenCache<S> {
        ScreenCache { uuids: vec![], system }
    }

    /// Returns a list containing the usable frame for each screen.
    ///
    /// This method must be called when there is an update to the screen
    /// configuration. It updates the internal cache so that calls to
    /// screen_spaces are fast.
    ///
    /// The main screen (if any) is always first. Note that there may be no
    /// screens.
    #[forbid(unsafe_code)] // called from test
    pub fn update_screen_config(
        &mut self,
        ns_screens: Vec<NSScreenInfo>,
    ) -> Option<(Vec<ScreenInfo>, CoordinateConverter)> {
        debug!("ns_screens={ns_screens:?}");

        let mut cg_screens = self.system.cg_screens().unwrap();
        debug!("cg_screens={cg_screens:?}");

        if ns_screens.len() != cg_screens.len() {
            // This occasionally happens when unlocking (#16). The events where
            // this happens have been extraneous, so ignoring them is fine.
            warn!(
                "Ignoring screen config change: There are {} ns_screens but {} cg_screens",
                ns_screens.len(),
                cg_screens.len(),
            );
            return None;
        }

        if cg_screens.is_empty() {
            return Some((vec![], CoordinateConverter::default()));
        };

        // Ensure that the main screen is always first.
        if let Some(main_screen_idx) =
            cg_screens.iter().position(|s| s.bounds.origin == CGPoint::ZERO)
        {
            cg_screens.swap(0, main_screen_idx);
        } else {
            warn!("Could not find main screen. cg_screens={cg_screens:?}");
        }

        self.uuids = cg_screens
            .iter()
            .map(|screen| self.system.uuid_for_rect(screen.bounds))
            .collect();

        // We want to get the visible_frame of the NSScreenInfo, but in CG's
        // top-left coordinates from NSScreen's bottom-left.
        // The main screen has origin (0, 0) in both coordinate systems.
        let converter = CoordinateConverter {
            screen_height: cg_screens[0].bounds.max().y,
        };

        let screens: Vec<ScreenInfo> = cg_screens
            .iter()
            .flat_map(|&CGScreenInfo { cg_id, .. }| {
                let Some(ns_screen) = ns_screens.iter().find(|s| s.cg_id == cg_id) else {
                    warn!("Can't find NSScreen corresponding to {cg_id:?}");
                    return None;
                };
                let converted = converter.convert_rect(ns_screen.visible_frame).unwrap();
                Some(ScreenInfo {
                    visible_frame: converted,
                    id: cg_id,
                    scale_factor: ns_screen.backing_scale_factor,
                })
            })
            .collect();
        Some((screens, converter))
    }

    /// Returns a list of the active spaces on each screen. The order
    /// corresponds to the screens returned by `screen_frames`.
    pub fn get_screen_spaces(&self) -> Vec<Option<SpaceId>> {
        self.uuids
            .iter()
            .map(|screen| unsafe {
                CGSManagedDisplayGetCurrentSpace(CGSMainConnectionID(), screen)
            })
            .map(|id| Some(SpaceId(NonZeroU64::new(id)?)))
            .collect()
    }
}

/// Converts between Quartz and Cocoa coordinate systems.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct CoordinateConverter {
    /// The y offset of the Cocoa origin in the Quartz coordinate system, and
    /// vice versa. This is the height of the first screen. The origins
    /// are the bottom left and top left of the screen, respectively.
    screen_height: f64,
}

/// Creates a `CoordinateConverter` that returns None for any conversion.
impl Default for CoordinateConverter {
    fn default() -> Self {
        Self { screen_height: f64::NAN }
    }
}

impl CoordinateConverter {
    pub fn convert_point(&self, point: CGPoint) -> Option<CGPoint> {
        if self.screen_height.is_nan() {
            return None;
        }
        Some(CGPoint::new(point.x, self.screen_height - point.y))
    }

    pub fn convert_rect(&self, rect: CGRect) -> Option<CGRect> {
        if self.screen_height.is_nan() {
            return None;
        }
        Some(CGRect::new(
            CGPoint::new(rect.origin.x, self.screen_height - rect.max().y),
            rect.size,
        ))
    }
}

#[allow(private_interfaces)]
pub trait System {
    fn cg_screens(&self) -> Result<Vec<CGScreenInfo>, CGError>;
    fn uuid_for_rect(&self, rect: CGRect) -> CFRetained<CFString>;
}

#[derive(Debug, Clone)]
struct CGScreenInfo {
    cg_id: ScreenId,
    bounds: CGRect,
}

#[derive(Debug, Clone)]
pub struct NSScreenInfo {
    pub frame: CGRect,
    pub visible_frame: CGRect,
    pub cg_id: ScreenId,
    pub backing_scale_factor: f64,
}

/// Gathers NSScreen information. Must be called on the main thread.
pub fn get_ns_screens(mtm: MainThreadMarker) -> Vec<NSScreenInfo> {
    NSScreen::screens(mtm)
        .iter()
        .flat_map(|s| {
            Some(NSScreenInfo {
                frame: s.frame(),
                visible_frame: s.visibleFrame(),
                cg_id: s.get_number().ok()?,
                backing_scale_factor: s.backingScaleFactor(),
            })
        })
        .collect()
}

pub struct Actual;
#[allow(private_interfaces)]
impl System for Actual {
    fn cg_screens(&self) -> Result<Vec<CGScreenInfo>, CGError> {
        const MAX_SCREENS: usize = 64;
        let mut ids: MaybeUninit<[CGDirectDisplayID; MAX_SCREENS]> = MaybeUninit::uninit();
        let mut count: u32 = 0;
        let ids = unsafe {
            let err = CGGetActiveDisplayList(
                MAX_SCREENS as u32,
                ids.as_mut_ptr() as *mut CGDirectDisplayID,
                &mut count,
            );
            if err != CGError::Success {
                return Err(err);
            }
            std::slice::from_raw_parts(ids.as_ptr() as *const u32, count as usize)
        };
        Ok(ids
            .iter()
            .map(|&cg_id| CGScreenInfo {
                cg_id: ScreenId(cg_id),
                bounds: CGDisplayBounds(cg_id),
            })
            .collect())
    }

    fn uuid_for_rect(&self, rect: CGRect) -> CFRetained<CFString> {
        // SAFETY: The call returns an owned (+1) CFString, per the copy rule.
        let uuid = unsafe { CGSCopyBestManagedDisplayForRect(CGSMainConnectionID(), rect) };
        let uuid = uuid.expect("CGSCopyBestManagedDisplayForRect returned NULL");
        unsafe { CFRetained::from_raw(uuid) }
    }
}

type CGDirectDisplayID = u32;

#[derive(Debug, Clone, PartialEq)]
pub struct ScreenInfo {
    pub visible_frame: CGRect,
    pub id: ScreenId,
    pub scale_factor: f64,
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Clone, Copy)]
pub struct ScreenId(CGDirectDisplayID);

impl ScreenId {
    #[cfg(test)]
    pub fn new(id: u32) -> ScreenId {
        ScreenId(id)
    }
}

pub trait NSScreenExt {
    fn get_number(&self) -> Result<ScreenId, ()>;
}
impl NSScreenExt for NSScreen {
    fn get_number(&self) -> Result<ScreenId, ()> {
        let desc = self.deviceDescription();
        match desc.objectForKey(ns_string!("NSScreenNumber")) {
            Some(val) if unsafe { msg_send![&*val, isKindOfClass:NSNumber::class() ] } => {
                let number: &NSNumber = unsafe { std::mem::transmute(val) };
                Ok(ScreenId(number.as_u32()))
            }
            val => {
                warn!(
                    "Could not get NSScreenNumber for screen with name {:?}: {:?}",
                    self.localizedName(),
                    val,
                );
                Err(())
            }
        }
    }
}

/// Returns the spaces of the display that currently shows `on_display`,
/// in the order macOS presents them.
///
/// Each display owns an independent set of spaces, so "Desktop 2" means the
/// second space *of one display*, matching what Mission Control shows. A
/// space number is only meaningful together with a display.
fn spaces_of_display(on_display: SpaceId) -> Option<Vec<SpaceId>> {
    for display in displays_with_user_spaces()? {
        if display.spaces.contains(&on_display) {
            return Some(display.spaces);
        }
    }
    None
}

/// The type macOS reports for an ordinary desktop, as opposed to the tile a
/// fullscreen or Split View app gets.
const SPACE_TYPE_USER: i64 = 0;

/// Whether a space with this reported type is one of the desktops macOS
/// numbers.
///
/// A space whose type is missing or unreadable counts as a desktop: wrongly
/// skipping one renumbers every desktop after it, which is worse than counting
/// a fullscreen tile we failed to identify.
fn is_user_space(ty: Option<i64>) -> bool {
    ty.is_none_or(|ty| ty == SPACE_TYPE_USER)
}

/// Each display's user-facing desktops, in the order macOS presents them.
///
/// Fullscreen and Split View apps occupy spaces of their own in the window
/// server's list, but Mission Control does not number them and neither does
/// the native Desktop shortcut. Counting them here would shift every desktop
/// number after the fullscreen window for as long as it stays fullscreen, so
/// they are filtered out and only `type == 0` spaces are numbered.
fn displays_with_user_spaces() -> Option<Vec<DisplaySpaces>> {
    let cid = unsafe { CGSMainConnectionID() };
    let space_info = unsafe { Retained::from_raw(CGSCopyManagedDisplaySpaces(cid))? };
    let mut displays = Vec::new();
    for screen in space_info {
        let Ok(screen) = screen.downcast::<NSDictionary>() else {
            continue;
        };
        let identifier = screen
            .valueForKey(ns_string!("Display Identifier"))
            .and_then(|id| id.downcast::<NSString>().ok())
            .map(|id| id.to_string());
        let Some(spaces) = screen
            .valueForKey(ns_string!("Spaces"))
            .and_then(|s| s.downcast::<NSArray>().ok())
        else {
            continue;
        };
        let ids: Vec<SpaceId> = spaces
            .iter()
            .filter_map(|space| {
                let space = space.downcast::<NSDictionary>().ok()?;
                let ty = space
                    .valueForKey(ns_string!("type"))
                    .and_then(|ty| ty.downcast::<NSNumber>().ok())
                    .map(|ty| ty.as_i64());
                let id: Retained<NSNumber> =
                    space.valueForKey(ns_string!("ManagedSpaceID"))?.downcast().ok()?;
                let id = NonZeroU64::new(id.as_u64()).map(SpaceId)?;
                is_user_space(ty).then_some(id)
            })
            .collect();
        displays.push(DisplaySpaces { identifier, spaces: ids });
    }
    Some(displays)
}

/// One display's user-facing desktops, with the identifier macOS knows it by.
struct DisplaySpaces {
    /// The window server's identifier for this display. A UUID for a real
    /// display; `None` if macOS did not report one.
    identifier: Option<String>,
    spaces: Vec<SpaceId>,
}

/// Names a display in config.
///
/// Display ids are reassigned when a display is reconnected, so config names a
/// display by the UUID the window server knows it by, or by `Builtin` for
/// whichever display macOS treats as the main one.
#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub enum DisplaySelector {
    /// The display macOS reports at the origin, whatever it is currently
    /// called. This is the built-in screen on a laptop.
    Builtin,
    /// A display by its window server UUID, matched case-insensitively.
    Uuid(String),
}

impl FromStr for DisplaySelector {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "builtin" | "main" | "primary" => Ok(DisplaySelector::Builtin),
            _ if is_display_uuid(s) => Ok(DisplaySelector::Uuid(s.to_owned())),
            _ => Err(format!(
                "expected \"builtin\" or a display UUID like \
                 \"84EFFBF0-1178-405C-9D64-3B574C1BE249\", found {s:?}"
            )),
        }
    }
}

/// Whether this is the UUID shape the window server uses for a display.
///
/// Checked so a misspelled alias such as `"buitlin"` is a config error rather
/// than a UUID that silently never matches any display.
fn is_display_uuid(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    groups.len() == 5
        && [8, 4, 4, 4, 12] == groups.iter().map(|g| g.len()).collect::<Vec<_>>()[..]
        && groups.iter().all(|g| g.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Deserialized from a plain string, so config writes `display = "builtin"` or
/// a UUID rather than a tagged table.
impl<'de> Deserialize<'de> for DisplaySelector {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        name.parse().map_err(serde::de::Error::custom)
    }
}

impl Serialize for DisplaySelector {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            DisplaySelector::Builtin => serializer.serialize_str("builtin"),
            DisplaySelector::Uuid(uuid) => serializer.serialize_str(uuid),
        }
    }
}

/// The window server UUID of the display macOS currently treats as the main
/// one, or `None` if no display reports itself as main.
///
/// Used to resolve `DisplaySelector::Builtin` by identity rather than by
/// position in the window server's display list. The two agree in a settled
/// configuration, but the list is rebuilt as displays come and go, so on wake
/// position alone can name a different display than the user meant.
fn main_display_uuid() -> Option<String> {
    const MAX_SCREENS: u32 = 64;
    let mut ids: MaybeUninit<[CGDirectDisplayID; MAX_SCREENS as usize]> = MaybeUninit::uninit();
    let mut count: u32 = 0;
    let ids = unsafe {
        let err = CGGetActiveDisplayList(
            MAX_SCREENS,
            ids.as_mut_ptr() as *mut CGDirectDisplayID,
            &mut count,
        );
        if err != CGError::Success {
            warn!(?err, "Could not list active displays");
            return None;
        }
        std::slice::from_raw_parts(ids.as_ptr() as *const CGDirectDisplayID, count as usize)
    };
    let main = ids.iter().copied().find(|&id| unsafe { CGDisplayIsMain(id) } != 0)?;
    // SAFETY: Both calls return owned (+1) references, per the create rule.
    // The UUID is released as soon as its string form has been copied out.
    unsafe {
        let uuid = CGDisplayCreateUUIDFromDisplayID(main)?;
        let string = CFUUIDCreateString(std::ptr::null(), uuid.as_ref());
        CFRelease(uuid.as_ptr() as *const c_void);
        let string = CFRetained::from_raw(string?);
        Some(string.to_string())
    }
}

/// Picks the display a selector names out of the window server's list.
///
/// Split out from `spaces_of_selected_display` so the matching rules can be
/// tested without a display attached. `main` is the UUID macOS currently
/// reports as the main display.
fn select_display(
    displays: Vec<DisplaySpaces>,
    selector: &DisplaySelector,
    main: Option<&str>,
) -> Option<Vec<SpaceId>> {
    let wanted = match selector {
        DisplaySelector::Builtin => main?,
        DisplaySelector::Uuid(uuid) => uuid.as_str(),
    };
    displays
        .into_iter()
        .find(|d| d.identifier.as_deref().is_some_and(|id| id.eq_ignore_ascii_case(wanted)))
        .map(|d| d.spaces)
}

/// The spaces of the display this selector names, in the order macOS presents
/// them, or `None` when that display is not currently connected.
pub fn spaces_of_selected_display(selector: &DisplaySelector) -> Option<Vec<SpaceId>> {
    let displays = displays_with_user_spaces()?;
    select_display(displays, selector, main_display_uuid().as_deref())
}

/// The result of looking up a desktop on a named display.
///
/// A disconnected display and a display without that many desktops are
/// different situations: the first is temporary and the caller may want to
/// wait for the display or fall back, while the second is a config error that
/// falling back would only hide.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum DesktopLookup {
    Found(SpaceId),
    /// The display is connected, but has no desktop at that position.
    NoSuchDesktop,
    /// The display is not currently connected.
    DisplayMissing,
}

/// The space at a user-facing position on a named display.
pub fn space_on_display(selector: &DisplaySelector, number: usize) -> DesktopLookup {
    let Some(spaces) = spaces_of_selected_display(selector) else {
        return DesktopLookup::DisplayMissing;
    };
    match number.checked_sub(1).and_then(|index| spaces.get(index)) {
        Some(&space) => DesktopLookup::Found(space),
        None => DesktopLookup::NoSuchDesktop,
    }
}

/// Returns the space at a user-facing position on the display that currently
/// shows `on_display`.
///
/// Space ids are assigned by the window server and change across reboots, so
/// config refers to spaces by this position instead. Numbering is per display:
/// with one desktop, only `1` resolves, and attaching a second display does
/// not renumber the first display's spaces.
pub fn space_with_number(number: usize, on_display: SpaceId) -> Option<SpaceId> {
    let spaces = spaces_of_display(on_display)?;
    spaces.get(number.checked_sub(1)?).copied()
}

/// Returns the user-facing number of the currently active space.
///
/// Numbered within its own display, the same way [`space_with_number`] reads a
/// number from config, so the status icon and a binding agree on what "Desktop
/// 2" means. Numbering across displays instead would label the second
/// display's only desktop with whatever the first display's count reached.
///
/// Note: This relies on private APIs and might break.
pub fn get_active_space_number() -> Option<usize> {
    let cid = unsafe { CGSMainConnectionID() };
    let active_id = NonZeroU64::new(unsafe { CGSGetActiveSpace(cid) }).map(SpaceId)?;
    for display in displays_with_user_spaces()? {
        if let Some(index) = display.spaces.iter().position(|&id| id == active_id) {
            return Some(index + 1);
        }
    }
    None
}

/// Utilities for querying the current system configuration. For diagnostic purposes only.
#[allow(dead_code)]
pub mod diagnostic {
    use super::*;

    pub fn cur_space() -> SpaceId {
        SpaceId(NonZeroU64::new(unsafe { CGSGetActiveSpace(CGSMainConnectionID()) }).unwrap())
    }

    pub fn visible_spaces() -> CFRetained<CFArray<SpaceId>> {
        // SAFETY: the array holds raw (non-retained) space id values, per the
        // copy rule and the way this private API is documented to behave.
        let arr = unsafe { CGSCopySpaces(CGSMainConnectionID(), CGSSpaceMask::ALL_VISIBLE_SPACES) };
        let arr = arr.expect("CGSCopySpaces returned NULL");
        unsafe { CFRetained::cast_unchecked(CFRetained::from_raw(arr)) }
    }

    pub fn all_spaces() -> CFRetained<CFArray<SpaceId>> {
        // SAFETY: as above.
        let arr = unsafe { CGSCopySpaces(CGSMainConnectionID(), CGSSpaceMask::ALL_SPACES) };
        let arr = arr.expect("CGSCopySpaces returned NULL");
        unsafe { CFRetained::cast_unchecked(CFRetained::from_raw(arr)) }
    }

    pub fn managed_displays() -> CFRetained<CFArray> {
        // SAFETY: the call returns an owned (+1) array, per the copy rule.
        let arr = unsafe { CGSCopyManagedDisplays(CGSMainConnectionID()) };
        let arr = arr.expect("CGSCopyManagedDisplays returned NULL");
        unsafe { CFRetained::from_raw(arr) }
    }

    pub fn managed_display_spaces() -> Retained<NSArray> {
        unsafe { Retained::from_raw(CGSCopyManagedDisplaySpaces(CGSMainConnectionID())) }.unwrap()
    }
}

// Based on https://github.com/asmagill/hs._asm.undocumented.spaces/blob/master/CGSSpace.h.
// Also see https://github.com/koekeishiya/yabai/blob/d55a647913ab72d8d8b348bee2d3e59e52ce4a5d/src/misc/extern.h.

/// Opaque stand-in for `CFUUIDRef`. The concrete type is not re-exported by
/// objc2-core-foundation, and only its string form is used here.
#[repr(C)]
struct CFUuid {
    _private: [u8; 0],
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGSMainConnectionID() -> c_int;
    fn CGSGetActiveSpace(cid: c_int) -> u64;
    fn CGSCopySpaces(cid: c_int, mask: CGSSpaceMask) -> Option<NonNull<CFArray>>;
    fn CGSCopyManagedDisplays(cid: c_int) -> Option<NonNull<CFArray>>;
    fn CGSCopyManagedDisplaySpaces(cid: c_int) -> *mut NSArray;
    fn CGSManagedDisplayGetCurrentSpace(cid: c_int, uuid: &CFString) -> u64;
    fn CGSCopyBestManagedDisplayForRect(cid: c_int, rect: CGRect) -> Option<NonNull<CFString>>;
    fn CGDisplayIsMain(display: CGDirectDisplayID) -> u32;
    fn CGDisplayCreateUUIDFromDisplayID(display: CGDirectDisplayID) -> Option<NonNull<CFUuid>>;
    fn CFUUIDCreateString(alloc: *const c_void, uuid: &CFUuid) -> Option<NonNull<CFString>>;
    fn CFRelease(cf: *const c_void);
}

bitflags! {
    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    #[repr(transparent)]
    struct CGSSpaceMask: c_int {
        const INCLUDE_CURRENT = 1 << 0;
        const INCLUDE_OTHERS  = 1 << 1;

        const INCLUDE_USER    = 1 << 2;
        const INCLUDE_OS      = 1 << 3;

        const VISIBLE         = 1 << 16;

        const CURRENT_SPACES = Self::INCLUDE_USER.bits() | Self::INCLUDE_CURRENT.bits();
        const OTHER_SPACES = Self::INCLUDE_USER.bits() | Self::INCLUDE_OTHERS.bits();
        const ALL_SPACES =
            Self::INCLUDE_USER.bits() | Self::INCLUDE_OTHERS.bits() | Self::INCLUDE_CURRENT.bits();

        const ALL_VISIBLE_SPACES = Self::ALL_SPACES.bits() | Self::VISIBLE.bits();

        const CURRENT_OS_SPACES = Self::INCLUDE_OS.bits() | Self::INCLUDE_CURRENT.bits();
        const OTHER_OS_SPACES = Self::INCLUDE_OS.bits() | Self::INCLUDE_OTHERS.bits();
        const ALL_OS_SPACES =
            Self::INCLUDE_OS.bits() | Self::INCLUDE_OTHERS.bits() | Self::INCLUDE_CURRENT.bits();
    }
}

#[cfg(test)]
mod test {
    use objc2_core_foundation::{CFRetained, CFString, CGPoint, CGRect, CGSize};

    use super::{
        CGError, CGScreenInfo, DisplaySelector, DisplaySpaces, NSScreenInfo, ScreenCache, ScreenId,
        SpaceId, System, is_user_space, select_display,
    };

    fn display(uuid: &str, spaces: &[u64]) -> DisplaySpaces {
        DisplaySpaces {
            identifier: Some(uuid.to_owned()),
            spaces: spaces.iter().map(|&id| SpaceId::new(id)).collect(),
        }
    }

    const BUILTIN: &str = "F8C4E36C-4313-4104-88FA-C9736A46352C";
    const EXTERNAL: &str = "84EFFBF0-1178-405C-9D64-3B574C1BE249";

    #[test]
    fn builtin_follows_the_main_display_not_the_list_order() {
        // macOS rebuilds the display list as displays come and go, so on wake
        // the main display is not always first. Resolving by position would
        // hand back the other display's spaces.
        let displays = vec![
            display(EXTERNAL, &[199]),
            display(BUILTIN, &[166, 167, 168]),
        ];
        assert_eq!(
            Some(vec![SpaceId::new(166), SpaceId::new(167), SpaceId::new(168)]),
            select_display(displays, &DisplaySelector::Builtin, Some(BUILTIN)),
        );
    }

    #[test]
    fn builtin_is_missing_when_no_display_is_main() {
        let displays = vec![display(BUILTIN, &[166])];
        assert_eq!(None, select_display(displays, &DisplaySelector::Builtin, None));
    }

    #[test]
    fn a_uuid_matches_regardless_of_case_and_position() {
        let displays = vec![display(BUILTIN, &[166]), display(EXTERNAL, &[199])];
        assert_eq!(
            Some(vec![SpaceId::new(199)]),
            select_display(
                displays,
                &DisplaySelector::Uuid(EXTERNAL.to_ascii_lowercase()),
                Some(BUILTIN),
            ),
        );
    }

    #[test]
    fn a_disconnected_display_resolves_to_nothing() {
        let displays = vec![display(BUILTIN, &[166])];
        assert_eq!(
            None,
            select_display(
                displays,
                &DisplaySelector::Uuid(EXTERNAL.to_owned()),
                Some(BUILTIN),
            ),
        );
    }

    struct Stub {
        cg_screens: Vec<CGScreenInfo>,
    }
    impl System for Stub {
        fn cg_screens(&self) -> Result<Vec<CGScreenInfo>, CGError> {
            Ok(self.cg_screens.clone())
        }
        fn uuid_for_rect(&self, _rect: CGRect) -> CFRetained<CFString> {
            CFString::from_static_str("stub")
        }
    }

    #[test]
    fn it_calculates_the_visible_frame() {
        let stub = Stub {
            cg_screens: vec![
                CGScreenInfo {
                    cg_id: ScreenId(1),
                    bounds: CGRect::new(CGPoint::new(3840.0, 1080.0), CGSize::new(1512.0, 982.0)),
                },
                CGScreenInfo {
                    cg_id: ScreenId(3),
                    bounds: CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(3840.0, 2160.0)),
                },
            ],
        };
        let ns_screens = vec![
            NSScreenInfo {
                cg_id: ScreenId(3),
                frame: CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(3840.0, 2160.0)),
                visible_frame: CGRect::new(CGPoint::new(0.0, 76.0), CGSize::new(3840.0, 2059.0)),
                backing_scale_factor: 2.0,
            },
            NSScreenInfo {
                cg_id: ScreenId(1),
                frame: CGRect::new(CGPoint::new(3840.0, 98.0), CGSize::new(1512.0, 982.0)),
                visible_frame: CGRect::new(CGPoint::new(3840.0, 98.0), CGSize::new(1512.0, 950.0)),
                backing_scale_factor: 2.0,
            },
        ];
        let mut sc = ScreenCache::new_with(stub);
        assert_eq!(
            vec![
                CGRect::new(CGPoint::new(0.0, 25.0), CGSize::new(3840.0, 2059.0)),
                CGRect::new(CGPoint::new(3840.0, 1112.0), CGSize::new(1512.0, 950.0)),
            ],
            sc.update_screen_config(ns_screens)
                .unwrap()
                .0
                .iter()
                .map(|s| s.visible_frame)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn rejected_screen_update_preserves_the_previous_cache() {
        let mut sc = ScreenCache::new_with(Stub {
            cg_screens: vec![CGScreenInfo {
                cg_id: ScreenId(1),
                bounds: CGRect::new(CGPoint::ZERO, CGSize::new(100.0, 100.0)),
            }],
        });
        let ns_screens = vec![NSScreenInfo {
            cg_id: ScreenId(1),
            frame: CGRect::ZERO,
            visible_frame: CGRect::ZERO,
            backing_scale_factor: 1.0,
        }];
        assert!(sc.update_screen_config(ns_screens.clone()).is_some());

        sc.system.cg_screens.push(CGScreenInfo {
            cg_id: ScreenId(2),
            bounds: CGRect::new(CGPoint::new(100.0, 0.0), CGSize::new(100.0, 100.0)),
        });
        assert!(sc.update_screen_config(ns_screens).is_none());
        assert_eq!(sc.uuids.len(), 1);
    }

    #[test]
    fn display_selector_rejects_a_misspelled_alias() {
        use std::str::FromStr;

        assert_eq!(
            DisplaySelector::from_str("builtin"),
            Ok(DisplaySelector::Builtin)
        );
        assert_eq!(DisplaySelector::from_str("MAIN"), Ok(DisplaySelector::Builtin));
        assert_eq!(
            DisplaySelector::from_str("84EFFBF0-1178-405C-9D64-3B574C1BE249"),
            Ok(DisplaySelector::Uuid(
                "84EFFBF0-1178-405C-9D64-3B574C1BE249".to_owned()
            ))
        );

        // A typo would otherwise become a UUID that never matches a display,
        // silently parking or falling back forever.
        assert!(DisplaySelector::from_str("buitlin").is_err());
        assert!(DisplaySelector::from_str("").is_err());
        assert!(DisplaySelector::from_str("not-a-uuid").is_err());
    }

    #[test]
    fn only_desktops_are_numbered() {
        // Mission Control numbers desktops but not the tile a fullscreen or
        // Split View app gets, so a number from config has to skip those.
        assert!(is_user_space(Some(0)));
        assert!(!is_user_space(Some(4)));

        // An unreadable type counts, so a desktop is never skipped by accident.
        assert!(is_user_space(None));
    }
}
