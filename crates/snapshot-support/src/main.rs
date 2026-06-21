// Copyright The Glide Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Freezes the current layout serialization as a versioned restore snapshot.
//!
//! Used by the release-please workflow: given the upcoming `--version`, it
//! copies `current.ron` to `<version>.ron` when the format changed since the
//! last saved version, and prints the path it wrote (nothing if unchanged) so
//! the workflow can `git add` it. The freeze decision lives in the library so
//! it stays unit-tested; this binary is just the CLI and the file copy.
//!
//! TODO: Fold this into a `cargo xtask` if one gets created.

use std::path::PathBuf;
use std::{fs, process};

use clap::Parser;

/// Freezes the current layout serialization as a versioned restore snapshot.
#[derive(Parser)]
#[command(name = "snapshot-support")]
struct Opt {
    /// Upcoming release version, e.g. `1.2.3`.
    #[arg(long)]
    version: String,
    /// Directory containing the restore snapshots.
    #[arg(long, default_value = "tests/snapshots")]
    dir: PathBuf,
}

fn main() {
    let opt = Opt::parse();

    match snapshot_support::snapshot_to_freeze(&opt.dir, &opt.version) {
        Ok(Some(target)) => {
            fs::copy(opt.dir.join("current.ron"), &target).expect("copy current.ron to snapshot");
            println!("{}", target.display());
        }
        Ok(None) => {}
        Err(err) => {
            eprintln!("error: {err}");
            process::exit(1);
        }
    }
}
