//! Locate and invoke external reference tools.
//!
//! Two-years-of-OSS-development design notes:
//!
//! 1. **Aliases matter.** `mkisofs` is `genisoimage` on Debian-derived
//!    distros, `xorriso` (with the `xorriso -as mkisofs` shim) on
//!    everything modern, and `hdiutil` on macOS. We resolve via an
//!    ordered alias list and remember which name actually worked so
//!    error messages cite the right binary.
//! 2. **Resolution is cached.** Tool lookup is `O(PATH entries)`,
//!    a meaningful cost when a CI job runs 100+ tests each invoking
//!    `Tool::resolve` independently. A `OnceLock`-backed cache keys
//!    on the tool's primary name.
//! 3. **`Skip` is a value, not a panic.** Tests return `Result<(),
//!    Skip>` and propagate skips via `?`. The default test harness
//!    prints `Err(Skip { … })` on failure; we live with that and
//!    rely on the `ISOMAGE_REQUIRE_TOOLS=1` CI gate to surface
//!    "should never have been skipped" cases.
//! 4. **Exit code matters but isn't the whole story.** Reference
//!    tools sometimes exit `0` after printing warnings; the
//!    `ToolOutput` returned exposes stdout, stderr, and status
//!    independently so tests can be strict where it matters.
//! 5. **Tool versions are recorded.** When `Tool::version()` is
//!    called, the output is captured and exposed for snapshot
//!    headers; this lets golden snapshots be re-pinned safely when
//!    a tool's output format changes.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;

/// A reference tool that may or may not be installed on the system.
///
/// Construct via [`Tool::new`] or the [`super::binaries::tools`]
/// registry. Calling [`Tool::resolve`] walks `$PATH` for the primary
/// name and each alias in order; the first hit wins.
///
/// `Tool` is `Copy` and intended to be declared `const`. Per-tool
/// state (resolution result) lives in a global cache so the
/// `const`-ness costs us nothing.
#[derive(Debug, Clone, Copy)]
pub struct Tool {
    primary: &'static str,
    aliases: &'static [&'static str],
}

impl Tool {
    /// Declare a tool by its canonical binary name.
    pub const fn new(primary: &'static str) -> Self {
        Self {
            primary,
            aliases: &[],
        }
    }

    /// Declare a tool with one or more alias binary names tried in
    /// order if the primary isn't on `$PATH`. The aliases should be
    /// listed most-modern-first; `xorriso` before `genisoimage`
    /// before `mkisofs`, for instance, so test runs against fresh
    /// images use the most-supported variant when available.
    pub const fn with_aliases(primary: &'static str, aliases: &'static [&'static str]) -> Self {
        Self { primary, aliases }
    }

    /// The canonical name (the one error messages quote).
    pub const fn primary(&self) -> &'static str {
        self.primary
    }

    /// All names the resolver will try, in priority order, with the
    /// primary first.
    pub fn all_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        std::iter::once(self.primary).chain(self.aliases.iter().copied())
    }

    /// Locate this tool on `$PATH`. Result is cached per-process.
    ///
    /// Returns `Some((name, path))` where `name` is the alias that
    /// worked (so callers can build args under the right CLI dialect)
    /// and `path` is the absolute binary path.
    pub fn resolve(&self) -> Option<Resolved> {
        type CacheEntry = (&'static str, Option<Resolved>);
        type CacheMap = std::sync::Mutex<Vec<CacheEntry>>;
        static CACHE: OnceLock<CacheMap> = OnceLock::new();
        let cache = CACHE.get_or_init(|| std::sync::Mutex::new(Vec::new()));

        // OnceLock-with-Vec is cheaper than HashMap for the dozen-ish
        // tools the registry contains. Iteration is O(N) but N ≤ 20.
        let mut guard = cache.lock().expect("tool cache poisoned");
        if let Some((_, cached)) = guard.iter().find(|(k, _)| *k == self.primary) {
            return cached.clone();
        }

        let resolved = self.do_resolve();
        guard.push((self.primary, resolved.clone()));
        resolved
    }

    fn do_resolve(&self) -> Option<Resolved> {
        let path_env = std::env::var_os("PATH")?;
        for name in self.all_names() {
            for dir in std::env::split_paths(&path_env) {
                let candidate = dir.join(name);
                if is_executable(&candidate) {
                    return Some(Resolved {
                        name,
                        path: candidate,
                    });
                }
            }
        }
        None
    }

    /// `true` iff the tool resolves on the current system. Cheaper
    /// than [`resolve`](Self::resolve) when you only need the boolean.
    pub fn is_available(&self) -> bool {
        self.resolve().is_some()
    }

    /// Resolve, or signal "skip this test" by returning `None`.
    ///
    /// This is the canonical pattern for round-trip tests:
    ///
    /// ```ignore
    /// # use isomage_test_common::*;
    /// #[test]
    /// fn my_round_trip() {
    ///     let Some(_) = tools::SGDISK.require_or_skip() else { return; };
    ///     // tool is available; do the real test
    /// }
    /// ```
    ///
    /// Unlike [`Skip::if_missing`] this never returns a `Skip` for
    /// the caller to propagate via `?` — that pattern looks like a
    /// test failure to Rust's default harness. The `Option<>` +
    /// `let-else` shape lets the test exit `Ok(())` cleanly.
    ///
    /// In strict mode (`ISOMAGE_REQUIRE_TOOLS=1`) a missing tool
    /// panics rather than returning `None`, so CI surfaces silently-
    /// dropped coverage as a hard failure.
    pub fn require_or_skip(&self) -> Option<Resolved> {
        if let Some(r) = self.resolve() {
            return Some(r);
        }
        let msg = format!(
            "tool {} not installed (also tried: {})",
            self.primary,
            self.aliases.join(", "),
        );
        if strict() {
            panic!(
                "ISOMAGE_REQUIRE_TOOLS=1 and {} — refusing to skip silently",
                msg
            );
        }
        eprintln!("skip: {msg}");
        None
    }

    /// Returns the tool's reported version string by running
    /// `<tool> --version` (or `-V` for tools whose flag differs).
    ///
    /// Best-effort: returns `None` if the tool isn't available, or
    /// if neither version flag yields a zero-exit result. The
    /// output is captured verbatim — callers parse it themselves.
    pub fn version(&self) -> Option<String> {
        let resolved = self.resolve()?;
        // Try `--version` first, then `-V`. Some old tools (e.g.
        // BSD `dd`) accept neither; we accept the None there.
        for flag in ["--version", "-V"] {
            let out = Command::new(&resolved.path)
                .arg(flag)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .ok()?;
            if out.status.success() {
                let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
                if text.trim().is_empty() {
                    text = String::from_utf8_lossy(&out.stderr).into_owned();
                }
                let line = text.lines().next().unwrap_or("").trim().to_string();
                if !line.is_empty() {
                    return Some(line);
                }
            }
        }
        None
    }

    /// Run the tool with `args`, returning its full output
    /// (stdout, stderr, and exit status). Stdin is closed; use
    /// [`run_with_stdin`](Self::run_with_stdin) if the tool needs input.
    ///
    /// Returns `ToolError::NotFound` if the tool isn't installed —
    /// tests should propagate via `?` and the caller decides
    /// whether to skip or fail.
    pub fn run<I, S>(&self, args: I) -> Result<ToolOutput, ToolError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.run_inner(args, &[], None)
    }

    /// Run with stdin piped from `input`. The tool sees EOF after.
    pub fn run_with_stdin<I, S>(&self, args: I, input: &[u8]) -> Result<ToolOutput, ToolError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.run_inner(args, &[], Some(input))
    }

    /// Run with extra environment variables on top of the inherited
    /// environment. Useful for tools that take config via env
    /// (e.g. `MTOOLSRC` for mtools).
    pub fn run_with_env<I, S, EI, EK, EV>(&self, args: I, env: EI) -> Result<ToolOutput, ToolError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
        EI: IntoIterator<Item = (EK, EV)>,
        EK: AsRef<OsStr>,
        EV: AsRef<OsStr>,
    {
        let env: Vec<(OsString, OsString)> = env
            .into_iter()
            .map(|(k, v)| (k.as_ref().to_owned(), v.as_ref().to_owned()))
            .collect();
        self.run_inner(args, &env, None)
    }

    fn run_inner<I, S>(
        &self,
        args: I,
        env: &[(OsString, OsString)],
        stdin: Option<&[u8]>,
    ) -> Result<ToolOutput, ToolError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.run_in_venue(args, env, stdin, &[], None)
    }

    /// Run the tool, optionally bind-mounting host paths into the
    /// venue's container (only meaningful when [`ToolVenue::Docker`]
    /// is in effect; ignored for [`ToolVenue::Path`]).
    ///
    /// `cwd` overrides the working directory of the spawned process.
    /// `None` inherits the current process's CWD (the default).
    ///
    /// [`super::round_trip::RoundTrip::try_build`] uses this to
    /// thread the round-trip tempdir into the container, so
    /// `$IMAGE` paths resolve identically inside and outside.
    pub fn run_in_venue<I, S>(
        &self,
        args: I,
        env: &[(OsString, OsString)],
        stdin: Option<&[u8]>,
        bind_mounts: &[&std::path::Path],
        cwd: Option<&Path>,
    ) -> Result<ToolOutput, ToolError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        use std::io::Write;

        let resolved = self.resolve().ok_or_else(|| ToolError::NotFound {
            primary: self.primary,
            searched: self.all_names().map(String::from).collect(),
        })?;

        // ToolVenue::current() reads ISOMAGE_TOOL_VENUE. Default
        // path-venue is `Command::new(resolved.path)`; Docker venue
        // wraps the same call in `docker run --rm -v ...`.
        let mut cmd = super::venue::ToolVenue::current().build_command(
            &resolved.path,
            resolved.name,
            bind_mounts,
        );
        cmd.args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        for (k, v) in env {
            cmd.env(k, v);
        }
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        let mut child = cmd.spawn().map_err(|e| ToolError::Spawn {
            tool: self.primary,
            source: e,
        })?;

        if let Some(input) = stdin {
            if let Some(mut child_stdin) = child.stdin.take() {
                child_stdin.write_all(input).map_err(|e| ToolError::Spawn {
                    tool: self.primary,
                    source: e,
                })?;
            }
        }

        let Output {
            status,
            stdout,
            stderr,
        } = child.wait_with_output().map_err(|e| ToolError::Spawn {
            tool: self.primary,
            source: e,
        })?;

        Ok(ToolOutput {
            tool: self.primary,
            invoked_as: resolved.name,
            status,
            stdout,
            stderr,
        })
    }
}

/// Successful tool resolution. `name` is the alias that hit on `$PATH`;
/// `path` is the absolute binary location.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub name: &'static str,
    pub path: PathBuf,
}

/// Captured result of running a tool. Reference-tool tests typically
/// check `status.success()` and one of `stdout`/`stderr`.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub tool: &'static str,
    pub invoked_as: &'static str,
    pub status: std::process::ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl ToolOutput {
    /// Assert exit-0 with a useful error message including stderr.
    pub fn assert_success(&self) {
        if !self.status.success() {
            panic!(
                "tool {} (invoked as {}) exited {:?}\n\
                 stdout: {}\n\
                 stderr: {}",
                self.tool,
                self.invoked_as,
                self.status.code(),
                String::from_utf8_lossy(&self.stdout),
                String::from_utf8_lossy(&self.stderr),
            );
        }
    }

    /// stdout as a UTF-8 string (lossy decode).
    pub fn stdout_string(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// stderr as a UTF-8 string (lossy decode).
    pub fn stderr_string(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }

    /// Convenience: assert success AND that the combined stdout/stderr
    /// contains `needle`. Useful for `qemu-img info ... | grep "file format: vpc"`.
    pub fn assert_contains(&self, needle: &str) {
        self.assert_success();
        if !self.stdout_string().contains(needle) && !self.stderr_string().contains(needle) {
            panic!(
                "tool {} output did not contain expected substring {needle:?}\n\
                 stdout: {}\n\
                 stderr: {}",
                self.tool,
                self.stdout_string(),
                self.stderr_string(),
            );
        }
    }
}

/// Failure modes for `Tool::run`. `NotFound` is the skip path; all
/// others are real errors that should fail the test.
#[derive(Debug)]
pub enum ToolError {
    NotFound {
        primary: &'static str,
        searched: Vec<String>,
    },
    Spawn {
        tool: &'static str,
        source: std::io::Error,
    },
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolError::NotFound { primary, searched } => {
                write!(
                    f,
                    "tool '{primary}' not on PATH (also tried: {})",
                    searched.join(", ")
                )
            }
            ToolError::Spawn { tool, source } => {
                write!(f, "failed to spawn {tool}: {source}")
            }
        }
    }
}

impl std::error::Error for ToolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ToolError::Spawn { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// "Skip this test because something it needed isn't available."
///
/// Returned by helpers like [`Skip::if_missing`] and the
/// [`super::round_trip::RoundTrip`] builder. Tests should declare
/// `#[test] fn …() -> Result<(), Skip>` and `?`-propagate.
///
/// In strict mode (`ISOMAGE_REQUIRE_TOOLS=1`), `Skip::if_missing`
/// panics instead of returning a `Skip`, which surfaces missing
/// tools as test failures.
#[derive(Debug)]
pub struct Skip {
    reason: String,
}

impl Skip {
    /// Skip with a custom reason string.
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    /// Resolve `tool` or skip (or panic, in strict mode). The eprintln
    /// happens on the skip path, so test logs show why coverage
    /// dropped.
    pub fn if_missing(tool: &Tool) -> Result<Resolved, Skip> {
        if let Some(resolved) = tool.resolve() {
            return Ok(resolved);
        }
        let msg = format!(
            "tool {} not installed (also tried: {})",
            tool.primary(),
            tool.aliases.join(", "),
        );
        if strict() {
            panic!(
                "ISOMAGE_REQUIRE_TOOLS=1 and {} — refusing to skip silently",
                msg
            );
        }
        eprintln!("skip: {msg}");
        Err(Skip { reason: msg })
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl std::fmt::Display for Skip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "skipped: {}", self.reason)
    }
}

impl std::error::Error for Skip {}

impl From<ToolError> for Skip {
    fn from(e: ToolError) -> Self {
        Skip {
            reason: e.to_string(),
        }
    }
}

/// `true` if `ISOMAGE_REQUIRE_TOOLS` is set to anything that parses
/// as a non-zero integer or any non-empty non-"0"/"false" string.
pub fn strict() -> bool {
    match std::env::var("ISOMAGE_REQUIRE_TOOLS") {
        Ok(v) => !matches!(v.trim().to_ascii_lowercase().as_str(), "" | "0" | "false"),
        Err(_) => false,
    }
}

/// Cross-platform "is this file executable?" check that's good
/// enough for our use case: any regular file on Windows, mode &
/// 0o111 on Unix. We don't bother with ACLs — tools live in `$PATH`
/// directories users have already vetted.
#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && (m.permissions().mode() & 0o111) != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}
