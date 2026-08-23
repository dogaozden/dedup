//! The two numbers the conformance protocol calls honesty fields.
//!
//! `opens` and `bytes_read` are incremented at the syscall and nowhere else.
//! Nothing here may ever be derived from a file's size: maintainers audit these
//! against a dyld-interpose shim, and a number that is computed rather than
//! counted is a number that can drift away from what the process really did.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
pub struct Effort {
    opens: AtomicU64,
    bytes_read: AtomicU64,
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

    /// Read `want` bytes, or whatever is left before the end of the file.
    ///
    /// read(2) is allowed to come back with less than it was asked for. Two
    /// identical files that happened to return different amounts would compare
    /// unequal and be split apart, so the loop is the difference between a
    /// missed duplicate and a found one.
    pub fn read_block(&self, file: &mut File, want: usize) -> io::Result<Vec<u8>> {
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
        Ok(block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_open_and_a_read_are_counted_once_each() {
        let effort = Effort::default();
        let mut file = effort.open(Path::new("Cargo.toml")).expect("readable");
        let block = effort.read_block(&mut file, 1 << 20).expect("readable");
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
            .read_block(&mut file, whole.len() + 4096)
            .expect("readable");
        assert_eq!(block.len(), whole.len());
        assert_eq!(effort.bytes_read(), whole.len() as u64);
    }
}
