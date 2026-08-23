# M6 — the spinning profile

Commissioned 2026-08-23, after a real-world run: a scan of a USB spinning
drive sat in the compare stage for tens of hours at ~1.5% CPU — seek-bound.
The tool's access patterns (tail probes, 8 sample windows, wide-group
interleaved compares) were tuned under a bytes-scored bench on a RAM disk,
where seeks are free. On spinning media they dominate. This milestone makes
the ALGORITHM device-aware, not just the pool sizes.

REWRITE.md §0 (fence) and §3 (inviolables) apply unchanged. Claim as M6 in
`rust/MILESTONES.md`; maintainer review is BLOCKING (this changes compare
correctness paths).

## The profile switch

`--profile flash|spinning|auto`, default `auto`:

- `auto` selects `spinning` only when the device is POSITIVELY identified as
  rotational; solid-state and unknown both get `flash` — i.e. today's
  behavior, byte-identical to the oracle. (External USB enclosures usually
  don't report; a user with a USB HDD forces `--profile spinning`. When the
  type is unknown, print one hint line suggesting it.)
- `flash` = the current pipeline, untouched. `conformance --gate all` must
  stay green with NO profile flag — that is the non-negotiable proof this
  milestone changed nothing by default.

## The spinning policy (design intent — implement, then verify as below)

1. **No tail probe.** The ends stage reads the head only; same-size files
   with identical heads fall through to deeper stages. More bytes, zero
   end-of-file seeks.
2. **No sample stage.** Deep-head survivors go straight to the full
   sequential compare. The 8 windows × N files seek pattern is the worst
   thing we do to a spinning drive.
3. **Sequential-friendly compare.** Raise the per-member read run to the
   spinning chunk (≥16 MiB) regardless of group width, and cap interleave:
   groups wider than K members (start with K=8, measure) are compared in
   passes over at most K files at a time, carrying the partition between
   passes. Reading a member in more than one pass is allowed — the profile
   trades bytes for seeks by design.
4. **One reader.** Spinning devices get a single sequential reader and
   inode-ordered scheduling within each stage.
5. **Cache untouched.** The scan cache is already the right fix for
   re-scans; the profile only changes cold-scan access patterns.

## Laws

- **Correctness is profile-independent.** Every profile must produce
  byte-identical GROUPS to the oracle on every tree — only access patterns,
  bytes_read, and opens may differ under `spinning`. §3's deletion
  inviolables (byte-proof, survivor guard, ProvenGroups) hold in every
  profile.
- **The bytes ceiling does not apply to `spinning`.** RBENCH scores the
  default profile only; RGOAL.md must say scored runs never pass
  `--profile`.
- **No claimed HDD speedups without a spinning-media measurement.** No
  rotational drive is available to agents (the only one on this machine is
  fenced). Implement, verify correctness and seek counts synthetically, and
  leave wall-clock validation to a user-supervised run. Say this plainly in
  the claim.

## Verification (what CAN be proven without an HDD)

- Differential vs the oracle on scratch SSD trees with `--profile spinning`
  FORCED: groups byte-identical to the oracle's on every tree, including
  adversarial ones — wide groups; groups whose members differ only in the
  last chunk; members that differ only across a pass boundary at width K;
  hardlink clusters; unreadables.
- **Seek accounting:** instrument non-contiguous read offsets per device
  (a counter beside opens/bytes_read). On a synthetic wide-group corpus,
  assert `spinning` issues a small fraction of `flash`'s seeks, and print
  both numbers in the claim.
- Unit tests on the pass-partitioning logic (no duplicate lost, no false
  merge, partition carried correctly between passes).
- `cargo test` / clippy `-D warnings` / fmt clean; `--gate all` green with
  no profile flag.
