//! The oracle's constants, at the clone's commit. These are settled
//! measurements — REWRITE.md §8 forbids re-tuning them — so they are copied
//! rather than reasoned about, and the comment on each says what it decides.

/// Stage 3's first pass: one page, enough to separate files that differ in
/// their header.
pub const HEAD_BYTES: usize = 4 * 1024;
/// How far stage 3 extends for the files a single page could not tell apart.
pub const DEEP_HEAD: usize = 64 * 1024;
/// One page from the far end, read before any more of the head: same-size
/// files that share a header are the ordinary case, and their last page is
/// where they differ.
pub const TAIL_BYTES: usize = 4 * 1024;
/// Per sample point in stage 4.
pub const SAMPLE_BYTES: usize = 64 * 1024;
/// Windows spread across the part stage 3 has not read.
pub const SAMPLE_POINTS: usize = 8;
/// Below this, just read it all: sampling a smaller file costs an open of its
/// own and saves less than that open is worth.
pub const SAMPLE_SPAN: u64 = 8 * 1024 * 1024;
/// Comparison chunk on flash.
///
/// Four megabytes was the obvious size and it is not the fast one. A narrow
/// group takes the whole of this — eight members of `bigheader` get 4 MB each,
/// 32 MB in flight — and neither the read nor the compare that follows it can
/// keep that in cache, so both go to DRAM. Measured in C at the comparison's
/// own shape, eight files read in lockstep and compared against a pivot:
/// 23.82 ms at 4 MB, 18.02 at 1 MB, **17.76 at 512 KB**, 18.60 at 256 KB.
/// Wide groups are unaffected — `COMPARE_BUDGET / members` already puts a
/// 227-member group at 564 KB, which is why this was invisible there.
pub const CHUNK: usize = 512 * 1024;
/// ...and on a spinning disk, where every extra file in a group costs a seek.
pub const SLOW_CHUNK: usize = 16 * 1024 * 1024;
/// Ceiling on bytes held for one group (or one wave) of the comparison.
pub const COMPARE_BUDGET: usize = 128 * 1024 * 1024;
/// ...which the chunk shrinks towards, but not below.
pub const MIN_CHUNK: usize = 128 * 1024;
/// How many file descriptors the staged pipeline will hold open between its
/// stages rather than reopening.
///
/// `widegroup` opens 5,442 times for 1,827 files — once in the ends probe,
/// once in the deep-head probe, once in the comparison — and an open is not
/// free: bare open-and-close over those files measures 5.54 ms on four
/// readers. Holding the descriptor across the stages pays that once.
///
/// The bound exists because a tree can be much larger than this one. macOS
/// here allows 61,440 descriptors per process and the soft limit the tool
/// inherits is 1,048,576, so 4,096 is nowhere near the ceiling; it is small
/// enough to be polite on a machine whose limits are lower, and past it the
/// stages reopen exactly as they did before.
pub const HANDLE_BUDGET: usize = 4096;

/// Files handed to a reader task at a time — one task per file spends more
/// time in the queue than in the read.
pub const HASH_BATCH: usize = 64;
/// A compare pass that strips off nothing but its own pivot, with this many
/// blocks still pending, means the pivot loop is going quadratic.
pub const PARTITION_FANOUT: usize = 256;
/// How many files the `spinning` profile interleaves in one comparison pass.
/// A round holds one `SLOW_CHUNK` per member, so eight of them is exactly
/// `COMPARE_BUDGET` — the widest a group can be read before the round-robin
/// between its members turns a sequential read into a seek storm.
pub const SPIN_INTERLEAVE: usize = 8;
/// How much of one member the `spinning` comparison holds while the rest of
/// its group streams past it.
///
/// This is the ceiling on a window, not its size: a sweep starts at
/// `SLOW_CHUNK` and grows by `PIVOT_GROWTH` until it reaches this, so a member
/// that parts from the pivot in its first megabytes costs exactly what the
/// lockstep round it replaces cost, and only a group that keeps agreeing pays
/// for the long windows that make it cheap. The spinning comparison holds one
/// window and one member's run — `PIVOT_WINDOW + PIVOT_RUN` — however wide the
/// group is, where `COMPARE_BUDGET` bounds a lockstep round, which holds one
/// run per member. M7-PIVOT.md sets the order of magnitude.
pub const PIVOT_WINDOW: usize = 256 * 1024 * 1024;
/// How fast a window grows towards `PIVOT_WINDOW`.
///
/// The bytes a pivot reads past the point its class falls apart are wasted, and
/// a window that grows by a factor of N holds that waste to N times the bytes
/// the class had already agreed on — so this is the price cap on being wrong
/// about a group, expressed as a multiple of how right it had been so far.
///
/// Four, not two, because the ceiling is sixteen chunks away and head
/// movements are what this profile is for: at two the ramp takes four windows
/// to reach it and a 256 MiB file costs five, where at four it costs three.
///
/// Measured by `evidence/scripts/pivot_growth.py`, which builds a variant of
/// this constant at two and reads the same corpora with both: four issues
/// 1.25x-1.40x fewer head movements and reads exactly the same bytes for them.
/// Nothing but head movements may move when this changes, and that script
/// asserts it.
pub const PIVOT_GROWTH: usize = 4;
/// How much of a window is read at a time.
///
/// Runs inside one member's window carry straight on from each other, so their
/// length costs no head movement at all: a window is one movement whether it
/// is read in one run or two thousand. What the length decides is how closely
/// a divergence is placed. A member that parts from the pivot stops at the end
/// of the run it parted in, and is compared against its own kind from the
/// start of that run — so a short run is fewer bytes read past a parting, and
/// fewer read a second time after one. On a group of four content classes that
/// fits in one `SLOW_CHUNK`, running at the chunk read 90 MB where running at
/// this reads 35.
///
/// It is `MIN_CHUNK` for the reason `MIN_CHUNK` is what it is: below this the
/// call starts to cost more than the bytes it carries.
pub const PIVOT_RUN: usize = MIN_CHUNK;
/// Ceiling on head bytes carried from stage 3 to stage 5 so the compare does
/// not read them a second time.
pub const RETAIN_BUDGET: usize = 128 * 1024 * 1024;
/// How many small-file candidates ride between checks of RETAIN_BUDGET.
pub const RETAIN_CHECK_SPAN: usize = HASH_BATCH * 64;

/// Bump when the stored columns change meaning. 4: a column for the head-only
/// digest the `spinning` profile reads, kept apart from the two-ends one.
pub const CACHE_SCHEMA: i64 = 4;
/// Entries not seen in this long are dropped.
pub const CACHE_TTL_DAYS: i64 = 180;
/// The coarsest granularity any target filesystem records mtime at — HFS+
/// stores whole seconds — so racily-clean is judged at whole seconds on every
/// volume, rather than asking each one how fine its clock is.
pub const CACHE_TICK_NS: i64 = 1_000_000_000;

pub const DEFAULT_MIN_SIZE: u64 = 10 * 1024 * 1024;
