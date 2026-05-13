---
pr: 19
slug: hfsplus-remaining-fixes
date: 2026-05-13
---

# PR #19 HFS+ reader — remaining Copilot fixes

## Issues addressed

### `detect()` cursor restore loses original error (medium)

`detect()` saved the cursor position, called `do_detect()`, then restored with
`r.seek(...)?`. If `do_detect` returned an error AND the restore seek also
failed, the `?` would propagate the seek error, discarding the detection
error. Changed to `let _ = r.seek(SeekFrom::Start(saved));` so the original
`do_detect` result is always returned regardless of whether the restore
succeeds.

### Error variants reused for unrelated catalog failures (medium)

`read_catalog_leaf_records` returned `Error::BadMagic` when catalog node 0
was not a B-tree header node, and `Error::BadVersion` when `node_size < 512`.
Both are structural catalog errors — nothing to do with the volume header
magic or version. Added a dedicated `Error::BadCatalog` variant ("HFS+
catalog B-tree structure is invalid") and used it for both cases.

### `make_hfsplus_image()` doc claimed `None` on tool failure (low)

The doc comment said "Returns `None` (causing the test to skip) if
`mkfs.hfsplus` is not installed **or fails**." But `build_bytes()` panics on
tool failure — `None` is returned only if `require_or_skip()` returns `None`
(tool absent). Fixed to: "Returns `None` if not installed. Panics if the
tool is present but fails to create the image."
