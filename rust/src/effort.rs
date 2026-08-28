//! The two numbers the conformance protocol calls honesty fields, and the
//! third the spinning profile is judged on.
//!
//! `opens` and `bytes_read` are incremented at the syscall and nowhere else.
//! Nothing here may ever be derived from a file's size: maintainers audit these
//! against a dyld-interpose shim, and a number that is computed rather than
//! counted is a number that can drift away from what the process really did.
//! `seeks` is held to the same rule — it counts reads that did not continue
//! where the last one stopped, from the offsets the process actually asked for.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

/// Where a read starts: which file, and how far into it.
///
/// The file is named by (dev, ino) rather than by a descriptor because a
/// descriptor is reused the moment its file is closed, and two reads either
/// side of a reuse would look like one continuous run.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct At {
    pub dev: u64,
    pub ino: u64,
    pub offset: u64,
}

#[derive(Default)]
pub struct Effort {
    opens: AtomicU64,
    bytes_read: AtomicU64,
    seeks: AtomicU64,
    /// Where the last read finished: which file, and the offset one past its
    /// last byte. A lock rather than two atomics so the pair is never torn —
    /// a half-updated pair would report a seek that did not happen.
    last: Mutex<Option<At>>,
    /// Whether anything is displaying the name below. Off for every piped run,
    /// every gate and every `--quiet` run, where it costs one relaxed load and
    /// a branch the predictor gets right every time — `pulse.rs`'s "off is
    /// off", kept here because `Effort` is the one thing already threaded to
    /// every read in the program, and giving `Compare` a `&Pulse` would have
    /// meant a field the deletion path has no progress display to fill.
    watching: AtomicBool,
    /// The file a reader most recently took up, for the progress display to
    /// name.
    ///
    /// Set where a read STARTS and not where a file is opened. An earlier
    /// draft recorded it in `open`, which is one line instead of seven and is
    /// wrong in exactly the case the display exists for: stages 3 and 4 hand
    /// their descriptors forward (`take_or_open`), so the comparison — the
    /// long stage, and the one a cloud file stalls in — opens nothing at all
    /// and the name stayed empty for the whole of it. Measured, not reasoned:
    /// on a 1.8 GB corpus, 14 painted frames named no file.
    ///
    /// Under parallel readers it is whichever reader started last, which is
    /// what M9-CLOUD.md asks for. For the case it exists for — a bar frozen on
    /// one file — it is exact, because a read that blocks has already recorded
    /// itself here before it blocks.
    reading: Mutex<String>,
}

impl Effort {
    /// Start recording the file each read starts on. Called by whichever
    /// display is going to show it, and by nothing else.
    pub fn watch_reads(&self) {
        self.watching.store(true, Ordering::Relaxed);
    }

    /// A reader is taking up `name` now.
    #[inline]
    pub fn reading(&self, name: &str) {
        if self.watching.load(Ordering::Relaxed) {
            let mut reading = self.reading.lock().unwrap_or_else(|held| held.into_inner());
            reading.clear();
            reading.push_str(name);
        }
    }

    /// The file a reader most recently took up, or empty when nothing is
    /// displaying or nothing has been read since the last stage began.
    pub fn now_reading(&self) -> String {
        self.reading
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .clone()
    }

    /// Forget it — a new stage has begun and has read nothing yet. Naming a
    /// file the previous stage read would be this display's own 1/t: something
    /// that keeps being shown after it stopped being true.
    pub fn forget_reading(&self) {
        self.reading
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .clear();
    }

    /// Open a file for reading and charge one open for it. A failed open is
    /// not charged — nothing was opened.
    pub fn open(&self, path: &Path) -> io::Result<File> {
        let file = File::open(path)?;
        self.opens.fetch_add(1, Ordering::Relaxed);
        Ok(file)
    }

    pub fn opens(&self) -> u64 {
        self.opens.load(Ordering::Relaxed)
    }

    pub fn bytes_read(&self) -> u64 {
        self.bytes_read.load(Ordering::Relaxed)
    }

    /// How many times the read stream jumped instead of carrying on.
    ///
    /// This is what a spinning disk charges for and flash does not, so it is
    /// the number the `spinning` profile is measured by: a read that starts
    /// anywhere but where the previous read stopped is one head movement,
    /// whether it moved within a file or to another file entirely. Concurrent
    /// readers interleave and so count against each other, which is the honest
    /// answer for one set of heads serving all of them.
    pub fn seeks(&self) -> u64 {
        self.seeks.load(Ordering::Relaxed)
    }

    /// Read `want` bytes from `at`, or whatever is left before the end of the
    /// file.
    ///
    /// read(2) is allowed to come back with less than it was asked for. Two
    /// identical files that happened to return different amounts would compare
    /// unequal and be split apart, so the loop is the difference between a
    /// missed duplicate and a found one.
    ///
    /// `at` says where the caller has positioned the handle. It is only ever
    /// read for the seek count; nothing about what is read depends on it.
    pub fn read_block(&self, file: &mut File, want: usize, at: At) -> io::Result<Vec<u8>> {
        self.charge_seek(at);
        let mut block = vec![0_u8; want];
        let mut filled = 0;
        while filled < want {
            let length = file.read(&mut block[filled..])?;
            if length == 0 {
                break;
            }
            self.bytes_read.fetch_add(length as u64, Ordering::Relaxed);
            filled += length;
        }
        block.truncate(filled);
        // Where this read actually stopped, not where it hoped to: a short
        // read leaves the handle at the end of what came back.
        let mut last = self.last.lock().expect("no reader panics");
        *last = Some(At {
            offset: at.offset + filled as u64,
            ..at
        });
        Ok(block)
    }

    fn charge_seek(&self, at: At) {
        let last = self.last.lock().expect("no reader panics");
        if *last != Some(at) {
            self.seeks.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(offset: u64) -> At {
        At {
            dev: 1,
            ino: 2,
            offset,
        }
    }

    #[test]
    fn an_open_and_a_read_are_counted_once_each() {
        let effort = Effort::default();
        let mut file = effort.open(Path::new("Cargo.toml")).expect("readable");
        let block = effort
            .read_block(&mut file, 1 << 20, at(0))
            .expect("readable");
        assert_eq!(effort.opens(), 1);
        assert_eq!(effort.bytes_read(), block.len() as u64);
        assert_eq!(block, std::fs::read("Cargo.toml").expect("readable"));
    }

    #[test]
    fn a_read_from_a_carried_descriptor_still_names_its_file() {
        // The trap this pins, found by running the tool rather than by
        // reading it. Recording the name inside `open` compiles, reads
        // sensibly, and leaves the display blank for the whole of the
        // comparison — because stages 3 and 4 hand their descriptors forward
        // (`stages::take_or_open`) and the comparison opens nothing at all.
        // On a 1.8 GB corpus that was 14 painted frames naming no file, in
        // exactly the stage a cloud placeholder stalls in.
        let effort = Effort::default();
        effort.watch_reads();

        // Stage 3: open, say what is being read, read.
        let mut file = effort.open(Path::new("Cargo.toml")).expect("readable");
        effort.reading("Cargo.toml");
        effort.read_block(&mut file, 16, at(0)).expect("readable");
        assert_eq!(effort.now_reading(), "Cargo.toml");

        // The stage boundary clears it, so no stage inherits the last file of
        // the one before it.
        effort.forget_reading();
        assert_eq!(effort.now_reading(), "");

        // Stage 5: the same descriptor, carried, and NOT reopened.
        effort.reading("Cargo.toml");
        effort.read_block(&mut file, 16, at(16)).expect("readable");
        assert_eq!(
            effort.now_reading(),
            "Cargo.toml",
            "a read from a carried descriptor left the progress display blank"
        );
        assert_eq!(
            effort.opens(),
            1,
            "the second read opened nothing, which is the whole reason \
             recording this in `open` was wrong"
        );
    }

    #[test]
    fn nothing_is_recorded_for_a_run_with_nothing_displaying() {
        // Off is off, as `pulse.rs` has it. Every piped run, every gate and
        // every --quiet run takes this path: one relaxed load, no lock, no
        // allocation, nothing to show.
        let effort = Effort::default();
        let mut file = effort.open(Path::new("Cargo.toml")).expect("readable");
        effort.reading("Cargo.toml");
        effort.read_block(&mut file, 16, at(0)).expect("readable");
        assert_eq!(
            effort.now_reading(),
            "",
            "a run with no display recorded a name anyway"
        );
        // ...and the counters the protocol grades are untouched by any of it.
        assert_eq!(effort.opens(), 1);
        assert_eq!(effort.bytes_read(), 16);
    }

    #[test]
    fn a_failed_open_is_not_charged() {
        let effort = Effort::default();
        assert!(effort.open(Path::new("no/such/file")).is_err());
        assert_eq!(effort.opens(), 0);
        assert_eq!(effort.bytes_read(), 0);
    }

    #[test]
    fn a_short_file_stops_at_its_end_rather_than_at_the_request() {
        let effort = Effort::default();
        let mut file = effort.open(Path::new("Cargo.toml")).expect("readable");
        let whole = std::fs::read("Cargo.toml").expect("readable");
        let block = effort
            .read_block(&mut file, whole.len() + 4096, at(0))
            .expect("readable");
        assert_eq!(block.len(), whole.len());
        assert_eq!(effort.bytes_read(), whole.len() as u64);
    }

    #[test]
    fn reading_straight_on_from_the_last_read_costs_no_seek() {
        let effort = Effort::default();
        let mut file = effort.open(Path::new("Cargo.toml")).expect("readable");
        let first = effort.read_block(&mut file, 8, at(0)).expect("readable");
        assert_eq!(first.len(), 8);
        assert_eq!(effort.seeks(), 1); // arriving at the file at all is one
        effort.read_block(&mut file, 8, at(8)).expect("readable");
        assert_eq!(effort.seeks(), 1);
        effort.read_block(&mut file, 8, at(16)).expect("readable");
        assert_eq!(effort.seeks(), 1);
    }

    #[test]
    fn jumping_within_a_file_or_to_another_file_costs_a_seek_either_way() {
        let effort = Effort::default();
        let mut file = effort.open(Path::new("Cargo.toml")).expect("readable");
        effort.read_block(&mut file, 8, at(0)).expect("readable");
        assert_eq!(effort.seeks(), 1);
        // Same file, but not where we stopped.
        effort.read_block(&mut file, 8, at(4096)).expect("readable");
        assert_eq!(effort.seeks(), 2);
        // Another file at the offset this one reached is still a jump.
        let mut other = effort.open(Path::new("Cargo.toml")).expect("readable");
        effort
            .read_block(
                &mut other,
                8,
                At {
                    dev: 1,
                    ino: 3,
                    offset: 4104,
                },
            )
            .expect("readable");
        assert_eq!(effort.seeks(), 3);
    }

    #[test]
    fn a_short_read_leaves_the_stream_where_the_bytes_stopped() {
        // Asking past the end and then continuing from the true end is not a
        // seek; continuing from where the request would have ended is.
        let effort = Effort::default();
        let whole = std::fs::read("Cargo.toml").expect("readable");
        let mut file = effort.open(Path::new("Cargo.toml")).expect("readable");
        effort
            .read_block(&mut file, whole.len() + 4096, at(0))
            .expect("readable");
        assert_eq!(effort.seeks(), 1);
        effort
            .read_block(&mut file, 8, at(whole.len() as u64))
            .expect("readable");
        assert_eq!(effort.seeks(), 1);
    }
}
