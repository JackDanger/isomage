# Security policy

`isomage` is a parser for untrusted binary data — CD/DVD/Blu-ray disc
images that arrive from arbitrary sources — and it can be asked to
write files derived from those images to the caller's filesystem. Both
the parser and the extractor are security-relevant surfaces.

## Supported versions

Only the most recent release on
[crates.io](https://crates.io/crates/isomage) is supported. Older
versions remain installable but won't get backports.

## Reporting a vulnerability

**Do not open a public GitHub issue for a vulnerability.** Use one of
these instead, in preferred order:

1. GitHub's [private security advisories](https://github.com/JackDanger/isomage/security/advisories/new)
   for this repo — the simplest path; routes directly to the maintainer.
2. Email `github@jackdanger.com` with `[isomage security]` in the
   subject. PGP available on request.

Please include:

- A description of the vulnerability and the harm it enables.
- A minimal reproduction — ideally a small ISO that triggers the
  behaviour, or a precise byte-level description if you can't share
  a sample.
- The version of `isomage` and the Rust toolchain you observed it on.

You will receive an acknowledgement within 7 days. A fix and a
coordinated-disclosure timeline will be agreed before any public
mention.

## What counts as a security issue

In scope:

- **Path traversal during `extract_node`** — writing outside the
  caller-supplied output directory. v2.0.0 added explicit guards for
  the names parsers can produce; we want to hear about any remaining
  path that escapes them.
- **Denial of service via crafted images** — panics, infinite loops,
  unbounded memory consumption, stack exhaustion. The parsers return
  `Err(...)` on invalid input by design; a panic is a bug.
- **Information disclosure** — leaking bytes outside the file the
  caller asked for, leaking host paths, leaking memory.
- **Memory safety** — undefined behaviour, out-of-bounds reads,
  use-after-free. The crate is pure safe Rust with **zero `unsafe`
  blocks**; UB would be very surprising and is high priority.

Out of scope:

- Issues that require write access to the input ISO or to a parent
  directory of the caller-supplied output (we trust the host
  filesystem).
- Dependency advisories already tracked by RustSec — `cargo audit`
  runs in CI; advisories on transitive deps will be addressed as
  they appear. Direct dependencies: zero, currently.

## Hardening summary

The current release hardens these specific surfaces:

- `extract_node` validates every directory-entry name against a
  conservative deny-list (`.`, `..`, `/`, `\`, NUL, empty) and
  re-checks that every resolved output path stays under the
  canonicalized output directory.
- `cat_node` matches `BrokenPipe` and returns `Ok(())` rather than
  letting it propagate to a panic in a downstream caller.
- Read loops keep file lengths as `u64` and clamp to a small `usize`
  chunk size before casting, so 32-bit targets can extract files
  larger than 4 GB without silent truncation.
- All parser code paths return `io::Error` on invalid input. If you
  find a panic on a crafted ISO, that's a bug worth reporting.

See [`CHANGELOG.md`](CHANGELOG.md) for the full list of
security-relevant changes per release.
