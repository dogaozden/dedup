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
use std::sync::atomic::{AtomicU64, Ordering};
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
}

impl Effort {
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
