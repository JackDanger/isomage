---
session: "zip-encrypted-stored"
timestamp: "2026-05-14T19:41:00Z"
model: gpt-5.5
---

## Human

Code review the whole project, open PRs with fixes, and keep changes tested,
necessary, and congruent with the rest of the crate.

## Assistant

### Key decisions

**Fix high-confidence behavior first**: The default and all-feature checks
were already clean, so I focused on a narrow correctness issue found during
manual review instead of broad style churn.

**Encrypted ZIP entries are unreadable data**: ZIP's general purpose bit 0
marks encrypted contents. Even when the compression method is "stored", the
bytes after the local file header are ciphertext, so exposing `file_location`
would make `cat_node` return encrypted bytes as if they were regular file
contents.

**Preserve existing API shape**: The parser already represents unsupported
compressed ZIP entries by keeping their size but omitting `file_location`.
Encrypted entries now use the same behavior, avoiding new public errors or
special cases.
