<!--
Thanks for the PR. Please walk this checklist before requesting review:

- [ ] `cargo build` and `cargo test` pass locally (incl. doc-tests).
- [ ] `cargo fmt --all` ran cleanly (CI gates on this).
- [ ] `cargo clippy --all-targets -- -D warnings` passes.
- [ ] If your change adds or alters any `pub` API, the rustdoc on it
      is updated and (ideally) a doc-test exercises the new shape.
- [ ] If you added a new dependency, the PR description explains why
      a pure-stdlib approach won't work — `isomage` is zero-dep by
      design (invariant 7 in the README).
- [ ] If you changed `src/` or `Cargo.toml`, you added a file under
      `prompts/YYYYMMDD-HHMMSS-<slug>.md` describing the prompts and
      key decisions. See `prompts/PROMPTLOG.md`, or invoke the
      `/promptlog` skill if you're using a Claude Code agent. The CI
      `Prompt log check` job will fail otherwise.
- [ ] If you added user-visible behaviour, you updated `README.md`
      and/or `CHANGELOG.md` under an Unreleased / Next section.
- [ ] If this is a security fix, please tell the maintainer first;
      see `SECURITY.md` for the coordinated-disclosure path.

Then delete this comment block and write the actual PR description below.
-->

## Summary

<!-- One paragraph: what changes, and why. -->

## How tested

<!-- What did you run? `cargo test`? A real ISO? Both? -->

## Related issues / PRs

<!-- e.g. closes #123 -->
