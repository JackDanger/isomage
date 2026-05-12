# Security policy

`isomage` is a parser for untrusted binary data (CD/DVD/Blu-ray disc
images) and an extractor that writes files to disk on the user's
behalf. Both surfaces are security-relevant.

## Supported versions

Security fixes land on `main` and ship in the next patch or minor
release. The most recent published release on
[crates.io](https://crates.io/crates/isomage) and
[GitHub Releases](https://github.com/JackDanger/isomage/releases)
is the only supported version. Older versions may still install
cleanly but won't receive backports.

## Reporting a vulnerability

**Do not open a public GitHub issue for a vulnerability.** Use one of
these instead, in preferred order:

1. GitHub's [private security advisories](https://github.com/JackDanger/isomage/security/advisories/new)
   for this repo. This is the easiest path and routes directly to the
   maintainer.
2. Email `github@jackdanger.com` with `[isomage security]` in the
   subject. PGP available on request.

Please include:

- A description of the vulnerability and the harm it enables.
- A minimal reproduction — ideally a small ISO that triggers the
  behaviour, or a precise byte-level description if you can't share
  a sample.
- The version of `isomage` and the platform you observed it on.

You will receive an acknowledgement within 7 days. A fix and a
coordinated disclosure timeline will be agreed before any public
mention.

## What counts as a security issue

The following are in scope:

- **Path traversal during `-x` / `extract_node`** — writing files
  outside the supplied output directory. `1.1.0` added explicit
  guards for the names parsers can produce; we want to hear about any
  remaining path that escapes them.
- **Denial of service via crafted images** — panics, infinite loops,
  unbounded memory consumption, or stack exhaustion triggered by a
  parsed image. The parsers return `Err(...)` on invalid input by
  design; a panic is a bug.
- **Information disclosure** — leaking bytes outside the file the
  user asked for, leaking host paths, or leaking memory.
- **Memory safety** — undefined behaviour, out-of-bounds reads,
  use-after-free. The crate is pure safe Rust; an unsafe-block-free
  UB would be very surprising and is a high-priority report.

Out of scope:

- Issues that require write access to the input ISO or to a parent
  directory of the output (we trust the user's filesystem).
- Issues caused by build-time tooling (clippy, rustfmt) unless they
  cause `isomage` itself to ship a vulnerability.
- Dependency advisories already tracked by RustSec — `cargo audit`
  runs in CI; advisories are addressed as they appear.

## Hardening summary

The current release hardens these specific surfaces:

- `extract_node` validates every directory-entry name against a
  conservative deny-list (`.`, `..`, `/`, `\`, NUL, empty) and
  re-checks that the resolved output path stays under the
  canonicalized output directory.
- `cat_node` matches `BrokenPipe` and returns `Ok` rather than
  propagating to a `panic!` somewhere up the stack.
- Read loops keep file lengths as `u64` and clamp to a small `usize`
  chunk size before casting, so 32-bit targets can extract files
  larger than 4 GB without silent truncation.
- All parser code paths return `io::Error` on invalid input rather
  than panicking. If you find a panic on a crafted ISO, that's a bug
  worth reporting.

See [`CHANGELOG.md`](CHANGELOG.md) for the full list of security-relevant
changes per release.
