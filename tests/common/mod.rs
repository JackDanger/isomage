//! Shared test infrastructure for `isomage`'s integration tests.
//!
//! This module is consumed by every file under `tests/` via
//! `mod common;`. It is **not** part of the public library API
//! and is never published to crates.io — `Cargo.toml`'s
//! `include = […]` allow-list excludes the entire `tests/` tree.
//!
//! # What lives here
//!
//! - [`tool`] — locating, invoking, and version-detecting reference
//!   tools (`xorriso`, `qemu-img`, `mkfs.vfat`, `sgdisk`, …).
//! - [`round_trip`] — the [`round_trip::RoundTrip`] builder that
//!   runs an external tool, returns the bytes it produced, and
//!   handles skip-or-fail policy uniformly.
//! - [`assertions`] — tree-shape assertions (`assert_path_exists`,
//!   `assert_partition_at_offset`, byte-equal-via-`cat_node`).
//! - [`snapshot`] — deterministic textual rendering of `TreeNode`
//!   with golden-file comparison; refresh with
//!   `ISOMAGE_UPDATE_SNAPSHOTS=1`.
//! - [`binaries`] — registry of every reference tool we exercise,
//!   with platform-aware alias lists (e.g. `mkisofs` is
//!   `genisoimage` on Debian, `xorriso` is the modern unifier).
//!
//! # Skip-or-fail policy
//!
//! Reference tools are not installed everywhere. The default is:
//!
//! - **Tool missing** → test prints `skip: <tool> not installed` to
//!   stderr and returns, exiting `Ok`.
//! - **Tool present, test fails** → test fails normally.
//!
//! In CI we want the **opposite** semantics so a missing tool doesn't
//! silently turn off coverage. Set `ISOMAGE_REQUIRE_TOOLS=1` and the
//! skip path panics instead. The `round-trip` CI job sets this.
//!
//! # Why an in-tree harness, not a published crate?
//!
//! Two reasons. First, the harness is exercised against the live
//! `isomage` API; if a refactor changes that API, the harness moves
//! in lockstep without a coordinated release. Second, contributors
//! can read it in one sitting — no docs.rs hop. If/when a sister
//! crate (e.g. an `isomage-cli`) wants the same harness, this
//! module graduates to a separate `isomage-test-utils` crate in a
//! workspace; the API surface is designed to support that move.
//!
//! Some imports/items are not used by every integration test
//! binary (each `tests/*.rs` is its own crate and pulls only the
//! symbols it touches). The `#[allow(dead_code)]` blanket on the
//! re-exports below is the canonical workaround.

// Each integration-test binary (each file under `tests/`) pulls only
// the symbols it uses; this module re-exports the surface area but
// every consumer touches a different subset. Silence the
// unused-warning blanket so we don't have to maintain per-test
// `#[allow]` annotations.
#![allow(dead_code, unused_imports)]

pub mod assertions;
pub mod binaries;
pub mod round_trip;
pub mod snapshot;
pub mod tool;

pub use binaries::tools;
pub use round_trip::RoundTrip;
pub use snapshot::assert_snapshot;
pub use tool::{Skip, Tool, ToolError, ToolOutput};
