<!--
Thanks for the PR. Please walk this checklist before requesting review:

- [ ] `cargo build` and `cargo test` pass locally.
- [ ] `cargo fmt --all` ran cleanly (CI gates on this).
- [ ] `cargo clippy --all-targets -- -D warnings` passes.
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

<!-- What did you run? `cargo test`? Manual `isomage -x …`? Both? -->

## Related issues / PRs

<!-- e.g. closes #123 -->
