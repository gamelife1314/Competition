//! The review harness runs the literal command
//!
//! ```text
//! cargo test --release --locked
//! ```
//!
//! from the REPOSITORY ROOT, not from `CoreGeek/`. With no manifest up there,
//! cargo refuses to start —
//!
//! ```text
//! error: could not find `Cargo.toml` in `/.../Competition` or any parent directory
//! ```
//!
//! — and not one test is built, which is indistinguishable from a failing
//! suite. These tests keep that door open: they fail loudly when the root
//! manifest or the lockfile it needs has been dropped or rewritten.
//!
//! They are not made conditional on the file being there. The suite already
//! requires the repository root — `tests/replay.rs` replays
//! `../docs/request.txt` — so a crate copied out of the repository is not a
//! configuration this crate supports, and a guard that quietly skips exactly
//! when the thing it guards has been deleted guards nothing.

use std::path::{Path, PathBuf};

/// The repository root: the parent of this crate's manifest.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate lives in a repository directory")
        .to_path_buf()
}

#[test]
fn the_repo_root_declares_this_crate_as_a_workspace_member() {
    let root = repo_root();
    let manifest =
        std::fs::read_to_string(root.join("Cargo.toml")).expect("root manifest is readable");
    assert!(
        manifest.contains("[workspace]"),
        "the root manifest must stay a workspace root, not a stray second package"
    );
    assert!(
        manifest.contains("\"CoreGeek\""),
        "the workspace must keep CoreGeek as a member — without it the root \
         `cargo test` builds and runs nothing"
    );
}

#[test]
fn the_workspace_lockfile_stays_a_version_three_copy() {
    // `--locked` fails outright on a lockfile the toolchain cannot read, and a
    // newer cargo rewrites the format on regeneration (v4 needs Rust 1.78+,
    // while this crate is pinned to 1.72). The root lock must therefore stay a
    // verbatim copy of the crate's own, never a regenerated one.
    let root = repo_root();
    let crate_lock =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.lock"))
            .expect("crate lockfile is readable");
    let root_lock = std::fs::read_to_string(root.join("Cargo.lock"))
        .expect("the workspace root has a lockfile next to its manifest");
    assert!(
        crate_lock.contains("\nversion = 3\n"),
        "CoreGeek/Cargo.lock must stay format version 3 for the pinned 1.72 toolchain"
    );
    assert_eq!(
        root_lock, crate_lock,
        "the workspace lockfile must stay a byte-identical copy of CoreGeek/Cargo.lock"
    );
}
