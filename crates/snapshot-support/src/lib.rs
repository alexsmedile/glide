// Copyright The Glide Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared logic for the layout restore snapshots in `tests/snapshots/`.
//!
//! This is deliberately platform-independent (no macOS dependencies) so it
//! builds and runs on the Linux release-please runner, while the version-sort
//! is also reused by the macOS test harness in the main crate.

use std::path::{Path, PathBuf};
use std::{fs, io};

/// Versioned snapshot files (e.g. `0.2.13.ron`), sorted oldest first.
///
/// `current.ron` is excluded because its stem does not parse as a version.
/// Files whose stem is not a dot-separated list of integers are ignored.
pub fn versioned_snapshots(dir: &Path) -> Vec<(Vec<u64>, PathBuf)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut snapshots: Vec<(Vec<u64>, PathBuf)> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("ron"))
        .filter_map(|path| {
            let stem = path.file_stem()?.to_str()?;
            let version = stem.split('.').map(|n| n.parse().ok()).collect::<Option<Vec<u64>>>()?;
            Some((version, path))
        })
        .collect();
    snapshots.sort();
    snapshots
}

/// Decides whether `current.ron` should be frozen as `<version>.ron`.
///
/// Returns the path to write when the current serialized format differs from
/// the most recently saved versioned snapshot (or none exists yet), or `None`
/// when the format is unchanged and no new snapshot is needed. The decision is
/// made purely from file contents so it can be exercised in unit tests; the
/// caller performs the copy.
pub fn snapshot_to_freeze(dir: &Path, version: &str) -> io::Result<Option<PathBuf>> {
    let current = fs::read_to_string(dir.join("current.ron"))?;
    if let Some((_, latest)) = versioned_snapshots(dir).pop()
        && fs::read_to_string(&latest)? == current
    {
        return Ok(None);
    }
    Ok(Some(dir.join(format!("{version}.ron"))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, contents: &str) {
        fs::write(dir.join(name), contents).unwrap();
    }

    #[test]
    fn versioned_snapshots_sorts_by_version_and_skips_current() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "current.ron", "x");
        write(dir.path(), "0.2.9.ron", "x");
        write(dir.path(), "0.2.13.ron", "x");
        write(dir.path(), "0.10.0.ron", "x");
        write(dir.path(), "notes.txt", "x");

        let names: Vec<_> = versioned_snapshots(dir.path())
            .into_iter()
            .map(|(_, path)| path.file_name().unwrap().to_str().unwrap().to_owned())
            .collect();
        // 0.2.13 must sort before 0.10.0 (numeric, not lexicographic), and
        // current.ron / notes.txt are excluded.
        assert_eq!(names, ["0.2.9.ron", "0.2.13.ron", "0.10.0.ron"]);
    }

    #[test]
    fn freezes_when_format_changed_since_latest() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "0.2.13.ron", "old");
        write(dir.path(), "current.ron", "new");

        let target = snapshot_to_freeze(dir.path(), "0.3.0").unwrap();
        assert_eq!(target, Some(dir.path().join("0.3.0.ron")));
    }

    #[test]
    fn skips_when_format_unchanged_since_latest() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "0.2.13.ron", "same");
        write(dir.path(), "current.ron", "same");

        assert_eq!(snapshot_to_freeze(dir.path(), "0.3.0").unwrap(), None);
    }

    #[test]
    fn compares_against_the_highest_version_not_the_last_listed() {
        let dir = tempfile::tempdir().unwrap();
        // An older snapshot matches current, but the newest one does not, so a
        // new snapshot is still needed.
        write(dir.path(), "0.2.13.ron", "current-format");
        write(dir.path(), "0.3.0.ron", "newer-format");
        write(dir.path(), "current.ron", "current-format");

        let target = snapshot_to_freeze(dir.path(), "0.3.1").unwrap();
        assert_eq!(target, Some(dir.path().join("0.3.1.ron")));
    }

    #[test]
    fn freezes_when_no_versioned_snapshots_exist() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "current.ron", "anything");

        let target = snapshot_to_freeze(dir.path(), "0.1.0").unwrap();
        assert_eq!(target, Some(dir.path().join("0.1.0.ron")));
    }
}
