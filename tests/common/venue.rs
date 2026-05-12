//! Where reference tools run: directly on `$PATH` (default), or
//! inside a sidecar Docker container (opt-in).
//!
//! # Motivation
//!
//! The default path-based invocation is fast, simple, and works
//! everywhere a contributor has the tools installed. It has two
//! limitations:
//!
//! 1. **Linux-only tools on macOS.** `sfdisk`, `mkntfs`, `debugfs`,
//!    `wimlib-imagex` either don't exist on macOS or are
//!    second-rate ports. macOS contributors can run round-trip
//!    tests against them only via a Linux container.
//!
//! 2. **Reproducibility.** `apt-get install -y sgdisk` gives
//!    whatever Ubuntu repo state happens to be live today. Six
//!    months from now, the same `apt-get` produces a different
//!    binary, and committed snapshot files start to diff.
//!
//! [`ToolVenue::Docker`] addresses both: it pins the tool versions
//! to a tagged image and lets macOS run Linux-only tools.
//!
//! # Opting in
//!
//! Set `ISOMAGE_TOOL_VENUE` to one of:
//!
//! - unset / empty / `path` → [`ToolVenue::Path`] (default)
//! - `docker:<image>` → [`ToolVenue::Docker`] using `<image>`
//!
//! Example:
//!
//! ```sh
//! ISOMAGE_TOOL_VENUE=docker:ghcr.io/jackdanger/isomage-test-tools:latest \
//!     cargo test --features mbr,gpt
//! ```
//!
//! # Bind-mount strategy
//!
//! The trick that keeps `$IMAGE` substitution working under Docker
//! is bind-mounting the host tempdir at *the same path* inside the
//! container. The container then reads/writes `/tmp/isomage-rt-…`
//! as if it were a local path, no translation needed.
//!
//! ## macOS host caveat
//!
//! On Linux hosts the bind-mount works without setup: the host
//! filesystem and the Docker daemon's filesystem are the same one,
//! and `-v /tmp/x:/tmp/x` is a direct rename. **On macOS hosts**
//! the Docker provider runs inside a Linux VM, and host paths only
//! propagate into the VM if they're explicitly shared:
//!
//! - **Docker Desktop**: shares `/Users`, `/tmp` (resolves to
//!   `/private/tmp`), `/Volumes`, `/private` out of the box. The
//!   default config Just Works for the round-trip harness.
//! - **Colima** (the OSS alternative): defaults to `mounts: []`
//!   in `~/.colima/default/colima.yaml`, which mounts only `$HOME`
//!   and `/tmp/colima` — *not* `/tmp` itself. Add the following
//!   to that file and `colima restart`:
//!   ```yaml
//!   mounts:
//!     - location: /tmp
//!       writable: true
//!   ```
//!
//! Without that, `docker run -v /tmp/foo:/tmp/foo` silently
//! mounts an empty directory inside the container, and the
//! round-trip tests fail with "file does not exist."
//!
//! # Performance note
//!
//! `docker run` cold-start is 100–500 ms. The Path venue invokes
//! the tool directly via `Command::new`, ~1 ms. Don't enable
//! Docker by default; it's a 100× slowdown on the self-tests.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where a reference tool runs.
///
/// This is selected once per process via [`ToolVenue::current`],
/// which reads the `ISOMAGE_TOOL_VENUE` env var. Tests don't need
/// to touch this directly — `Tool::run` consults it automatically.
#[derive(Debug, Clone)]
pub enum ToolVenue {
    /// Invoke the tool directly on the host's `$PATH`.
    Path,
    /// Invoke the tool via `docker run` against the given image.
    /// Any extra bind-mounts the test wants on top of the default
    /// "current dir + temp dir" set go in `extra_mounts`.
    Docker {
        image: String,
        extra_mounts: Vec<(PathBuf, PathBuf)>,
    },
}

impl ToolVenue {
    /// Read the venue selection from `ISOMAGE_TOOL_VENUE`. Returns
    /// [`ToolVenue::Path`] if the env var is unset, empty, or
    /// literally `"path"`.
    pub fn current() -> Self {
        match std::env::var("ISOMAGE_TOOL_VENUE") {
            Ok(v) => Self::parse(&v),
            Err(_) => Self::Path,
        }
    }

    /// Optional override for where the round-trip harness creates
    /// its tempdir. Default ([`ToolVenue::Path`]) returns `None`,
    /// meaning `std::env::temp_dir()`.
    ///
    /// Under [`ToolVenue::Docker`] we force `/tmp` because:
    ///
    /// - Linux CI runners default to `/tmp` anyway (no-op).
    /// - macOS hosts default to `/var/folders/...`, which Docker
    ///   Desktop / Colima do **not** share with the VM by default.
    ///   Bind-mounting an unshared path silently produces a
    ///   read-only or non-existent path inside the container. `/tmp`
    ///   *is* shared by every Mac Docker provider out of the box.
    ///
    /// Tests then bind-mount `/tmp/isomage-rt-...` at the same path
    /// inside the container, and `$IMAGE` substitution works
    /// transparently.
    pub fn tempdir_root(&self) -> Option<&'static Path> {
        match self {
            ToolVenue::Path => None,
            ToolVenue::Docker { .. } => Some(Path::new("/tmp")),
        }
    }

    /// Parse a string in the format documented at the top of this
    /// module. Exposed for tests of the venue itself; production
    /// callers go through [`current`](Self::current).
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        if s.is_empty() || s.eq_ignore_ascii_case("path") {
            return Self::Path;
        }
        if let Some(image) = s.strip_prefix("docker:") {
            return Self::Docker {
                image: image.to_string(),
                extra_mounts: Vec::new(),
            };
        }
        // Unknown venue value — fall back to Path with a one-line
        // warning so tests don't silently bypass their intended venue.
        eprintln!("warning: ISOMAGE_TOOL_VENUE={s:?} is not recognised; using path",);
        Self::Path
    }

    /// Build a `Command` to invoke the tool. For [`Path`](Self::Path),
    /// this is just `Command::new(absolute_path)`. For [`Docker`](Self::Docker),
    /// this assembles a `docker run` invocation that bind-mounts the
    /// supplied tempdir at the same path inside the container.
    ///
    /// `tool_path` is the absolute path to the tool *on the host*
    /// (from `Tool::resolve`); under Docker we ignore it and use the
    /// alias name to find the binary inside the container's `$PATH`.
    ///
    /// `invoked_name` is the alias that hit on `$PATH` — under Path
    /// it's just informational; under Docker it's what we invoke.
    ///
    /// `mounts` is the set of host paths to bind into the container.
    /// At minimum this should include the round-trip tempdir; for
    /// Path it's ignored.
    pub fn build_command(&self, tool_path: &Path, invoked_name: &str, mounts: &[&Path]) -> Command {
        match self {
            ToolVenue::Path => Command::new(tool_path),
            ToolVenue::Docker {
                image,
                extra_mounts,
            } => {
                let mut cmd = Command::new("docker");
                cmd.arg("run")
                    .arg("--rm")
                    // `-i` keeps stdin attached so RoundTrip::stdin works.
                    .arg("-i");
                for m in mounts {
                    cmd.arg("-v")
                        .arg(format!("{}:{}", m.display(), m.display()));
                }
                for (host, container) in extra_mounts {
                    cmd.arg("-v")
                        .arg(format!("{}:{}", host.display(), container.display()));
                }
                // Map the invoking user so files created inside the
                // container have host-side ownership. Unix-only; on
                // Windows hosts `docker --user` is ignored, which is
                // fine because Windows is out of scope.
                #[cfg(unix)]
                {
                    // SAFETY: getuid/getgid are POSIX functions that
                    // take no arguments and return a u32; calling them
                    // from any thread is well-defined.
                    let uid = unsafe { libc_getuid() };
                    let gid = unsafe { libc_getgid() };
                    cmd.arg("--user").arg(format!("{uid}:{gid}"));
                }
                cmd.arg(image).arg(invoked_name);
                cmd
            }
        }
    }
}

// Tiny libc wrappers we don't want to add a dep for. Two FFI calls
// gated exclusively behind `#[cfg(unix)]`. `getuid`/`getgid` are the
// most portable POSIX calls there are; they take no arguments and
// return a u32 each.
#[cfg(unix)]
extern "C" {
    fn getuid() -> u32;
    fn getgid() -> u32;
}

#[cfg(unix)]
unsafe fn libc_getuid() -> u32 {
    getuid()
}

#[cfg(unix)]
unsafe fn libc_getgid() -> u32 {
    getgid()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_path_default() {
        matches!(ToolVenue::parse(""), ToolVenue::Path);
        matches!(ToolVenue::parse("path"), ToolVenue::Path);
        matches!(ToolVenue::parse("PATH"), ToolVenue::Path);
    }

    #[test]
    fn parse_docker_image() {
        if let ToolVenue::Docker { image, .. } =
            ToolVenue::parse("docker:ghcr.io/jackdanger/isomage-test-tools:v1")
        {
            assert_eq!(image, "ghcr.io/jackdanger/isomage-test-tools:v1");
        } else {
            panic!("expected Docker variant");
        }
    }

    #[test]
    fn parse_unknown_falls_back_to_path() {
        matches!(ToolVenue::parse("kubernetes"), ToolVenue::Path);
    }

    #[test]
    fn path_command_uses_absolute_path() {
        let v = ToolVenue::Path;
        let cmd = v.build_command(Path::new("/usr/bin/sgdisk"), "sgdisk", &[]);
        // Command's program is what we asked.
        assert_eq!(cmd.get_program(), OsStr::new("/usr/bin/sgdisk"));
    }

    #[test]
    fn docker_command_invokes_docker() {
        let v = ToolVenue::Docker {
            image: "test:latest".into(),
            extra_mounts: vec![],
        };
        let cmd = v.build_command(
            Path::new("/usr/bin/sgdisk"),
            "sgdisk",
            &[Path::new("/tmp/work")],
        );
        assert_eq!(cmd.get_program(), OsStr::new("docker"));
        let args: Vec<_> = cmd.get_args().collect();
        assert!(args.iter().any(|a| *a == OsStr::new("run")));
        assert!(args.iter().any(|a| *a == OsStr::new("--rm")));
        assert!(args.iter().any(|a| *a == OsStr::new("-v")));
        assert!(args.iter().any(|a| *a == OsStr::new("test:latest")));
        assert!(args.iter().any(|a| *a == OsStr::new("sgdisk")));
    }
}
