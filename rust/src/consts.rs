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
pub const CHUNK: usize = 4 * 1024 * 1024;
/// ...and on a spinning disk, where every extra file in a group costs a seek.
pub const SLOW_CHUNK: usize = 16 * 1024 * 1024;
/// Ceiling on bytes held for one group (or one wave) of the comparison.
pub const COMPARE_BUDGET: usize = 128 * 1024 * 1024;
/// ...which the chunk shrinks towards, but not below.
pub const MIN_CHUNK: usize = 128 * 1024;
/// Files handed to a reader task at a time — one task per file spends more
/// time in the queue than in the read.
pub const HASH_BATCH: usize = 64;
/// A compare pass that strips off nothing but its own pivot, with this many
/// blocks still pending, means the pivot loop is going quadratic.
pub const PARTITION_FANOUT: usize = 256;
/// Ceiling on head bytes carried from stage 3 to stage 5 so the compare does
/// not read them a second time.
pub const RETAIN_BUDGET: usize = 128 * 1024 * 1024;
/// How many small-file candidates ride between checks of RETAIN_BUDGET.
pub const RETAIN_CHECK_SPAN: usize = HASH_BATCH * 64;

/// Bump when the stored columns change meaning. 3: every row carries the time
/// it was written, which is what the racily-clean rule reads.
pub const CACHE_SCHEMA: i64 = 3;
/// Entries not seen in this long are dropped.
pub const CACHE_TTL_DAYS: i64 = 180;
/// The coarsest granularity any target filesystem records mtime at — HFS+
/// stores whole seconds — so racily-clean is judged at whole seconds on every
/// volume, rather than asking each one how fine its clock is.
pub const CACHE_TICK_NS: i64 = 1_000_000_000;

pub const DEFAULT_MIN_SIZE: u64 = 10 * 1024 * 1024;
