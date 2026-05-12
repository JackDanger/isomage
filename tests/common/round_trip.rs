//! Build disk images with external tools, parse them with `isomage`,
//! assert the trees match.
//!
//! # The pattern
//!
//! Every format we support needs four kinds of test:
//!
//! 1. **External → internal**: a reference tool produces an image,
//!    `isomage` parses it, we assert the tree matches.
//! 2. **Internal → external**: `isomage` writes an image (Phase 4),
//!    a reference tool's fsck/info accepts it.
//! 3. **Internal → internal**: `isomage` round-trips through write+read
//!    on the same image and gets the same tree.
//! 4. **Cross-tool**: a reference image is read by both `isomage` and
//!    a different reference tool; their trees agree.
//!
//! [`RoundTrip`] is the builder for kinds (1) and (4). Kinds (2) and
//! (3) reuse [`super::tool::Tool::run`] directly because they don't
//! need the source-tree / image-out scaffolding.
//!
//! # Path substitution
//!
//! When you pass `"$IMAGE"` in [`RoundTrip::arg`]s, the harness
//! replaces it at run time with the absolute path of a managed
//! tempfile. After the tool exits, the file's bytes are read and
//! returned. Other substitutions:
//!
//! - `$SRC_DIR` — populated source directory (via
//!   [`RoundTrip::source_file`]).
//! - `$OUT_DIR` — secondary output directory if the tool writes
//!   multiple files.
//! - `$TMP` — the bare tempdir root (rarely needed; prefer the named
//!   forms above so the harness can clean up safely).
//!
//! # Tempdir cleanup
//!
//! The tempdir is owned by an internal [`tempfile::TempDir`] and
//! removed on `Drop`, including on test failure (panics unwind, the
//! `Drop` runs). If a test crashes the process (`SIGSEGV`), the
//! tempdir leaks; `cargo test` runners on CI clean `/tmp/`
//! periodically so this is acceptable.
//!
//! # Determinism
//!
//! Reference tools sometimes embed timestamps in their output (e.g.
//! `xorriso` stamps creation time into the ISO PVD). Tests that
//! depend on exact byte-equality should set `SOURCE_DATE_EPOCH=0`
//! via [`RoundTrip::env`]; most modern tools honour it.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::common::tool::{Skip, Tool, ToolOutput};

/// Builder for "run an external tool, capture the bytes it produced."
///
/// See module docs for the substitution language. The builder is
/// consumed by [`RoundTrip::build`].
pub struct RoundTrip {
    name: String,
    tool: Option<&'static Tool>,
    args: Vec<OsString>,
    stdin: Option<Vec<u8>>,
    env: Vec<(OsString, OsString)>,
    source_files: Vec<(PathBuf, Vec<u8>)>,
    image_preallocate: Option<u64>,
}

impl RoundTrip {
    /// Start a new round-trip scenario. `name` is purely for log
    /// output — it appears in skip messages and tempdir paths so
    /// post-mortem inspection (`ls -la /tmp/isomage-rt-*`) is sane.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            tool: None,
            args: Vec::new(),
            stdin: None,
            env: Vec::new(),
            source_files: Vec::new(),
            image_preallocate: None,
        }
    }

    /// The tool that produces the image.
    pub fn with(mut self, tool: &'static Tool) -> Self {
        self.tool = Some(tool);
        self
    }

    /// Append one CLI argument. Use `"$IMAGE"`, `"$SRC_DIR"`,
    /// `"$OUT_DIR"`, or `"$TMP"` for path placeholders.
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Append several CLI arguments at once.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        for a in args {
            self.args.push(a.into());
        }
        self
    }

    /// Pipe `input` into the tool's stdin (e.g. an `sfdisk` directive).
    pub fn stdin(mut self, input: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(input.into());
        self
    }

    /// Set an environment variable for the tool. The tool inherits
    /// our environment otherwise. Common pattern:
    /// `.env("SOURCE_DATE_EPOCH", "0")` for reproducible output.
    pub fn env(mut self, k: impl Into<OsString>, v: impl Into<OsString>) -> Self {
        self.env.push((k.into(), v.into()));
        self
    }

    /// Stage a file in the source-tree directory. Path is relative
    /// to `$SRC_DIR`. Parent directories are created as needed.
    /// Order is preserved (some tools care about insertion order,
    /// e.g. `mkisofs`'s file ordering affects the ISO layout).
    pub fn source_file(
        mut self,
        relpath: impl Into<PathBuf>,
        contents: impl Into<Vec<u8>>,
    ) -> Self {
        self.source_files.push((relpath.into(), contents.into()));
        self
    }

    /// Pre-allocate `$IMAGE` to `bytes` zeros before invoking the
    /// tool. Required for partition-table editors like `sfdisk` and
    /// `sgdisk` that don't grow their target file.
    pub fn image_size(mut self, bytes: u64) -> Self {
        self.image_preallocate = Some(bytes);
        self
    }

    /// Run the tool and return the bytes of `$IMAGE`.
    ///
    /// **Panics** if the tool isn't installed, on non-zero exit, or
    /// on any I/O failure. Tests should guard with
    /// [`super::tool::Tool::require_or_skip`] *before* calling this:
    ///
    /// ```ignore
    /// let Some(_) = tools::SFDISK.require_or_skip() else { return; };
    /// let bytes = RoundTrip::new("…").with(&tools::SFDISK).args([…]).build().into_bytes();
    /// ```
    ///
    /// Use [`try_build`](Self::try_build) instead if your test needs
    /// to distinguish "tool missing" (skip) from "tool failed"
    /// (assertion failure) at the call site.
    pub fn build(self) -> RoundTripOutput {
        self.try_build().expect("RoundTrip::build")
    }

    /// Fallible variant: returns `Err(Skip)` if the tool isn't
    /// installed and propagates other errors via panic-on-error
    /// inside the tool invocation. Useful when a single test runs
    /// against multiple tools and wants to record which one was used.
    pub fn try_build(self) -> Result<RoundTripOutput, Skip> {
        let tool = self.tool.expect(
            "RoundTrip::build called without RoundTrip::with(tool); add `.with(&tools::FOO)`",
        );
        // Resolve once to fail-fast on missing tool with a Skip; the
        // actual binary path is rediscovered inside `tool.run_*` (it's
        // cached, so the cost is a single hashmap lookup).
        let _ = Skip::if_missing(tool)?;

        // Choose tempdir root from the active venue so Docker on
        // macOS (default tempdir /var/folders/... is unshared) lands
        // on /tmp instead. Path venue keeps std's default.
        let prefix = format!("isomage-rt-{}-", sanitize(&self.name));
        let tmp = match super::venue::ToolVenue::current().tempdir_root() {
            // tempfile signature is `with_prefix_in(prefix, dir)`.
            Some(root) => TempDir::with_prefix_in(&prefix, root),
            None => TempDir::with_prefix(&prefix),
        }
        .expect("failed to create tempdir");

        let src_dir = tmp.path().join("src");
        let out_dir = tmp.path().join("out");
        let image_path = tmp.path().join("image.bin");
        fs::create_dir_all(&src_dir).expect("create src dir");
        fs::create_dir_all(&out_dir).expect("create out dir");

        // Stage source files.
        for (rel, contents) in &self.source_files {
            let full = src_dir.join(rel);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).expect("create parent dir");
            }
            fs::write(&full, contents).expect("write source file");
        }

        // Pre-allocate the image file if requested. We use
        // `set_len` to make a sparse file — the file system maps
        // unwritten ranges to zero pages, which is what partition
        // editors want.
        if let Some(size) = self.image_preallocate {
            let f = fs::File::create(&image_path).expect("create image file");
            f.set_len(size).expect("preallocate image");
        }

        // Substitute placeholders.
        let substituted: Vec<OsString> = self
            .args
            .iter()
            .map(|a| substitute(a, &image_path, &src_dir, &out_dir, tmp.path()))
            .collect();

        // Run the tool. We use `run_in_venue` (not plain `run`) so
        // ISOMAGE_TOOL_VENUE=docker:... can bind-mount the tempdir
        // and the substituted `$IMAGE` / `$SRC_DIR` paths resolve
        // identically inside and outside the container.
        let bind_mounts: &[&std::path::Path] = &[tmp.path()];
        let env_slice: Vec<(OsString, OsString)> = self.env.clone();
        let stdin_slice: Option<&[u8]> = self.stdin.as_deref();
        let output = tool.run_in_venue(
            substituted.iter().map(|s| s.as_os_str()),
            &env_slice,
            stdin_slice,
            bind_mounts,
        );

        let output = output.map_err(Skip::from)?;
        // Strict-by-default: a non-zero exit from the tool is a
        // test failure, not a skip. Tests that *expect* failure
        // should use `tool.run(...)` directly.
        if !output.status.success() {
            panic!(
                "round-trip {:?}: tool {} exited {:?}\n\
                 invoked as: {}\n\
                 args: {:?}\n\
                 stdout: {}\n\
                 stderr: {}",
                self.name,
                tool.primary(),
                output.status.code(),
                output.invoked_as,
                self.args,
                output.stdout_string(),
                output.stderr_string(),
            );
        }

        // Read image bytes. If the tool wrote elsewhere, the test
        // should pull them via `output.tempdir_path()` / read
        // explicitly; the common path is "tool wrote to $IMAGE".
        let bytes = if image_path.exists() {
            fs::read(&image_path).expect("read image file")
        } else {
            Vec::new()
        };

        // Keep TempDir alive in RoundTripOutput so paths it returned
        // stay valid for the test body. Drop happens at end of test.
        Ok(RoundTripOutput {
            bytes,
            tool_output: output,
            tempdir: tmp,
            image_path,
            src_dir,
            out_dir,
        })
    }

    /// Convenience: build and discard the tool output / tempdir,
    /// returning just the image bytes. Panics on tool failure;
    /// callers should guard with `require_or_skip()` first.
    pub fn build_bytes(self) -> Vec<u8> {
        self.build().into_bytes()
    }
}

/// What `RoundTrip::build` returns. The `tempdir` field keeps the
/// temporary directory alive for the test body; paths returned via
/// `image_path()` / `src_dir()` / `out_dir()` are valid until this
/// struct drops.
pub struct RoundTripOutput {
    bytes: Vec<u8>,
    tool_output: ToolOutput,
    tempdir: TempDir,
    image_path: PathBuf,
    src_dir: PathBuf,
    out_dir: PathBuf,
}

impl RoundTripOutput {
    /// Bytes the tool wrote to `$IMAGE`.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Consume self and return owned image bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    pub fn tool_output(&self) -> &ToolOutput {
        &self.tool_output
    }

    pub fn image_path(&self) -> &Path {
        &self.image_path
    }

    pub fn src_dir(&self) -> &Path {
        &self.src_dir
    }

    pub fn out_dir(&self) -> &Path {
        &self.out_dir
    }

    pub fn tempdir(&self) -> &Path {
        self.tempdir.path()
    }

    /// Re-write `$IMAGE` from arbitrary bytes (for the
    /// "isomage writes, reference tool verifies" Phase-4 pattern).
    pub fn overwrite_image(&self, bytes: &[u8]) -> std::io::Result<()> {
        let mut f = fs::File::create(&self.image_path)?;
        f.write_all(bytes)?;
        f.sync_all()
    }
}

/// Replace `$IMAGE` / `$SRC_DIR` / `$OUT_DIR` / `$TMP` tokens
/// anywhere they appear in `arg`. Both `$VAR` and `${VAR}` forms
/// are recognised; for the bare-`$VAR` form, the variable name
/// ends at the first non-identifier character so `$IMAGES` (no such
/// variable) is left untouched.
///
/// Common usage: `of=$IMAGE` (`dd`-style key=value), `$SRC_DIR/a`
/// (path-suffix), `${IMAGE}_backup` (unambiguous when followed by
/// alphanumerics).
fn substitute(arg: &OsStr, image: &Path, src: &Path, out: &Path, tmp: &Path) -> OsString {
    let Some(s) = arg.to_str() else {
        // Non-UTF-8 arg — pass through unchanged. We could expand
        // via OsString concat but no real test needs it.
        return arg.to_owned();
    };
    let expanded = expand_vars(s, |name| match name {
        "IMAGE" => Some(image.to_string_lossy().into_owned()),
        "SRC_DIR" => Some(src.to_string_lossy().into_owned()),
        "OUT_DIR" => Some(out.to_string_lossy().into_owned()),
        "TMP" => Some(tmp.to_string_lossy().into_owned()),
        _ => None,
    });
    OsString::from(expanded)
}

/// Expand `$VAR` and `${VAR}` tokens via `lookup`. Unknown variables
/// are left as-is so unrelated `$`s in args (e.g. `$1` in a shell
/// script tool input) survive. `$$` is *not* treated as an escape;
/// no test currently needs it and adding it can be a follow-up.
fn expand_vars(input: &str, mut lookup: impl FnMut(&str) -> Option<String>) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            out.push(bytes[i] as char);
            i += 1;
            continue;
        }
        // Braced form: ${NAME}
        if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            if let Some(end) = bytes[i + 2..].iter().position(|&b| b == b'}') {
                let name = &input[i + 2..i + 2 + end];
                if let Some(val) = lookup(name) {
                    out.push_str(&val);
                    i += 2 + end + 1;
                    continue;
                }
            }
            // No closing brace or unknown var — leave verbatim.
            out.push('$');
            i += 1;
            continue;
        }
        // Bare form: $NAME, where NAME = [A-Z_][A-Z0-9_]*
        let mut end = i + 1;
        while end < bytes.len() {
            let c = bytes[end];
            let is_ident =
                c.is_ascii_uppercase() || c == b'_' || (end > i + 1 && c.is_ascii_digit());
            if !is_ident {
                break;
            }
            end += 1;
        }
        if end == i + 1 {
            // No identifier after `$`.
            out.push('$');
            i += 1;
            continue;
        }
        let name = &input[i + 1..end];
        if let Some(val) = lookup(name) {
            out.push_str(&val);
        } else {
            out.push_str(&input[i..end]);
        }
        i = end;
    }
    out
}

/// Tempdir prefix sanitizer: anything outside `[A-Za-z0-9._-]`
/// becomes `_`. Keeps tempdir names readable in `ls -la /tmp`.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::expand_vars;

    fn lk(name: &str) -> Option<String> {
        match name {
            "IMAGE" => Some("/tmp/img".into()),
            "SRC_DIR" => Some("/tmp/src".into()),
            _ => None,
        }
    }

    #[test]
    fn expand_exact() {
        assert_eq!(expand_vars("$IMAGE", lk), "/tmp/img");
    }

    #[test]
    fn expand_in_key_value() {
        assert_eq!(expand_vars("of=$IMAGE", lk), "of=/tmp/img");
    }

    #[test]
    fn expand_with_suffix_path() {
        assert_eq!(expand_vars("$SRC_DIR/a", lk), "/tmp/src/a");
    }

    #[test]
    fn unknown_variable_passes_through() {
        assert_eq!(expand_vars("$NOPE/x", lk), "$NOPE/x");
    }

    #[test]
    fn longer_name_does_not_partial_match() {
        // $IMAGES should NOT match $IMAGE.
        assert_eq!(expand_vars("$IMAGES", lk), "$IMAGES");
    }

    #[test]
    fn braced_form() {
        assert_eq!(expand_vars("${IMAGE}_backup", lk), "/tmp/img_backup");
    }

    #[test]
    fn lone_dollar_passes_through() {
        assert_eq!(expand_vars("price is $5", lk), "price is $5");
    }
}
