// Copyright The Glide Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Restore-compatibility snapshots.
//!
//! `tests/snapshots/` holds a serialized layout per supported version. The tests
//! here restore every snapshot to catch format changes that break restoration,
//! and compare the current code's output against `current.ron` so an
//! intentional format change is noticed before it ships.
//!
//! Workflow when the serialized format changes:
//!  1. Run with `GLIDE_BLESS_SNAPSHOTS=1` to rewrite `current.ron`.
//!  2. The release-please workflow adds `current.ron` to the release PR as
//!     `<version>.ron` (only when the format changed since the last saved
//!     version) so the old format stays covered by the restore test. (This
//!     support is dropped once a version is no longer worth restoring.)

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::{env, fs};

use objc2_core_foundation::{CGPoint, CGRect, CGSize};

use super::testing::{Apps, make_windows};
use super::{Event, Reactor};
use crate::actor::layout::LayoutManager;
use crate::sys::screen::{CoordinateConverter, SpaceId};

fn snapshot_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

/// Builds a representative layout deterministically: two apps with several
/// windows each on one space. The exact contents matter less than that the same
/// inputs always produce byte-identical output, so a format change is visible.
fn canonical_serialized() -> String {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
    reactor.handle_event(Event::ScreenParametersChanged {
        frames: vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.))],
        spaces: vec![Some(SpaceId::new(1))],
        scale_factors: vec![2.0],
        converter: CoordinateConverter::default(),
        on_screen: Default::default(),
    });
    reactor.handle_events(apps.make_app(1, make_windows(3)));
    reactor.handle_events(apps.make_app(2, make_windows(2)));
    apps.simulate_until_quiet(&mut reactor);
    reactor.layout.serialize_to_string()
}

/// Every committed snapshot must still deserialize and keep its windows.
#[test]
fn all_snapshots_restore() {
    let dir = snapshot_dir();
    let mut restored = 0;
    for entry in fs::read_dir(&dir).expect("snapshot dir should exist") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("ron") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let serialized = fs::read_to_string(&path).unwrap();
        let layout: LayoutManager = ron::from_str(&serialized)
            .unwrap_or_else(|e| panic!("failed to restore snapshot {name}: {e}"));
        assert!(
            !layout.all_windows().is_empty(),
            "snapshot {name} restored no windows; its window mapping was lost"
        );
        restored += 1;
    }
    assert!(restored > 0, "no snapshots found in {}", dir.display());
}

/// Detects when the serialized format changes so a new snapshot can be saved.
#[test]
fn current_serialization_is_unchanged() {
    let serialized = canonical_serialized();
    assert_eq!(
        serialized,
        canonical_serialized(),
        "canonical layout serialization is not deterministic"
    );

    let path = snapshot_dir().join("current.ron");

    if env::var_os("GLIDE_BLESS_SNAPSHOTS").is_some() {
        fs::write(&path, &serialized).unwrap();
        return;
    }

    let expected = fs::read_to_string(&path).unwrap_or_default();
    if serialized != expected {
        fs::write(path.with_extension("ron.new"), &serialized).unwrap();
        panic!(
            "Serialized layout format changed (wrote current.ron.new).\n\
             If intended, re-run with GLIDE_BLESS_SNAPSHOTS=1 to update current.ron, and once the \
             new format ships copy it to tests/snapshots/<version>.ron to keep restore coverage."
        );
    }
}

/// Guards the property the restore fixtures rely on: the current format differs
/// from the older one, so `all_snapshots_restore` is actually exercising the
/// backward-compatible path rather than trivially round-tripping.
#[test]
fn legacy_snapshot_differs_from_current_format() {
    let (_, path) = snapshot_support::versioned_snapshots(&snapshot_dir())
        .into_iter()
        .next()
        .expect("expected at least one versioned snapshot");
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let legacy = fs::read_to_string(&path).unwrap();
    let current: LayoutManager = ron::from_str(&legacy).unwrap();
    assert_ne!(
        legacy,
        current.serialize_to_string(),
        "{name} is already in the current format; replace it with a genuine old snapshot"
    );
    let _: BTreeSet<_> = current.all_windows();
}
