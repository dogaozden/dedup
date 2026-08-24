# M7 — the pivot-window compare

Commissioned 2026-08-24, from a measured wall: stage 5 on the
maintainer's spinning drive moves ~44 MB/s against a ~110–130 MB/s
sequential ceiling, because lockstep comparison round-robins the head
between every member of a group. Claim as M7 in `rust/MILESTONES.md`;
the maintainer review is BLOCKING (`rust/REVIEW-M7.md`).

## The change

Under `--profile spinning` ONLY, stage 5 stops interleaving. Per group:

- Read the PIVOT member sequentially, a window at a time (window = a
  RAM budget on the order of 256 MiB, tunable constant beside the other
  budgets in consts.rs), holding the window's bytes.
- For each window, read the SAME range of each other member
  sequentially and compare in memory against the held pivot bytes.
- A member that diverges from the pivot leaves the pivot's class. Two
  members that both diverge from the pivot may still equal EACH OTHER:
  the residue set is compared among itself the same way (first residue
  member becomes the next pivot), recursively, until every class is
  settled. Final classes must be exactly the byte-equality classes —
  identical to what lockstep produces, proven by test.

The flash profile keeps today's parallel lockstep untouched: on flash
the interleave costs nothing and the parallelism wins, and RBENCH's
pinned scenarios must not move.

## Laws

- Byte-exactness unchanged: every reported group still traces to every
  byte compared. No digest ever settles anything.
- Group membership byte-identical to the lockstep path on every input.
  This includes split ORDER effects: whatever ordering/keeper semantics
  the current splitter feeds downstream must be preserved.
- `effort` counters keep their meanings (bytes_read = bytes actually
  read; seeks should FALL on spinning — that is the point).
- Deletion path untouched. `verify_attested` untouched.
- Descriptor carry-through (the held-`File` table) must compose or be
  explicitly bypassed on the spinning path with a comment saying why.
- Memory: the window budget is the cap. A 1,800-member widegroup must
  not hold 1,800 windows — only the pivot's window is held; members
  stream against it one at a time.

## Verification

- Unit tests: divergence in first/middle/last window and AT window
  boundaries; two members equal to each other but different from the
  pivot (the residue-regroup case, in both same-window and
  different-window divergence variants); tiny files smaller than one
  window; groups of 2, 3, and many; a group where everything is unique.
- A property test: random same-size corpora, spinning-path classes ==
  lockstep-path classes, byte for byte.
- Conformance `--gate all` ({stage} form) 33/33.
- RBENCH sanity: flash scenarios within noise vs main (the harness
  runs flash; a moved number here is a defect).
- Seek evidence: on a generated multi-group corpus with the spinning
  profile, capture opens/bytes_read/seeks vs main — bytes equal, seeks
  materially lower. Wall-clock validation on real rust is the
  maintainer's Passport session, NOT yours: never touch /Volumes.
