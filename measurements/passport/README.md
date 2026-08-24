# Passport measurement session — 2026-08-23

The supervised spinning-media session SPIN.md required before the M6
profile may claim HDD wall-clock numbers. All runs executed by the
maintainer personally (never agents) with tools/passport-measure.sh,
read-only, against a 5 TB WD Passport (APFS on USB, `Solid State: Info
not available`, no SMART — auto-detection cannot identify it as
spinning, confirming M6's never-guess design).

Raw transcripts: the five `passport-measure-*.txt` files here.
Binary: the merged Rust tool at main 48a7470 (post round-1, M6, M5.1).

## The fair cold 2×2 — subtree "Old usb", 920 files, --min-size 1 MiB

Drive ejected and physically re-attached before each cold run.

| run | walk (find) | scan |
|---|---|---|
| spinning, cold (20:03) | 1.39 s | **8.32 s** |
| flash, cold (20:05) | 2.05 s | **6.66 s** |

The `find` walk alone varied 1.39→2.05 s run to run, so the profile gap
is inside the drive's own variance band. Verdict: **statistical tie —
no spinning-profile win demonstrated on this subtree.** Not a
refutation either: 920 files yielded almost no same-size candidates, so
the compare stage — the only stage the profile changes — barely ran.

Earlier same-session (non-cold) runs showing why eject discipline
matters: spinning appeared 12× faster (0.47 s) when it ran second on
flash's warmed page cache (19:58 transcript). That number is cache
flattery, recorded here so nobody quotes it.

## Validated findings

1. **The cache is the HDD product.** Warm re-scan of the subtree:
   0.014 s, and it SURVIVES eject + re-attach (cache lives on the Mac,
   keyed by device/inode/mtime — 20:03 and 20:05 transcripts, run
   immediately after remount). Cold scan ~7–8 s vs warm 0.014 s:
   re-checking spinning media is effectively free.
2. **Cold walks are seek-bound physics.** Whole-drive `find` observed
   at ~140 IOPS of ~4 KB metadata reads (kernel iostat on the physical
   disk, 19:54 attempt, aborted). At ~660 files/s cold, a full
   multi-million-file drive walks in tens of minutes for every tool.
3. **The M6 unknown-media hint works.** Cache-on runs printed the
   "volume does not say whether it spins" advisory exactly when
   intended.
4. **Design question opened, not answered:** the spinning profile reads
   with ONE reader (queue depth 1). Flash's 8 parallel readers give the
   drive's native command queueing room to reorder seeks, and cold
   flash beat cold spinning here. Before any round-3 HDD work, test
   whether a small queue depth (2–8 readers) beats one reader on real
   rust even for the seek-averse strategy.

## Still owed

- A duplicate-heavy subtree (many same-size candidates — the workload
  that originally sat at "95%, comparing") to give the compare-stage
  profile a real test.
- The whole-drive overnight run (spinning profile, caffeinate).
