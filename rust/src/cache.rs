//! Digests remembered from earlier scans, one sqlite file per volume.
//!
//! Scanning the same volume twice reads the same bytes twice, and on a large
//! disk that reading is the whole cost. Remembering each file's digests against
//! (inode, size, mtime_ns) lets the narrowing stages skip everything nothing
//! has touched since last time.
//!
//! The rule that makes this safe to ship in a deletion tool: **the cache may
//! only ever narrow.** A remembered digest can save a read. It can never be the
//! reason a file is removed. Every group matched from memory is read and
//! compared before anything in it is deleted — see `verify_attested`, which is
//! the other half of this feature and not optional.
//!
//! Rows are keyed by inode, which is what the hardlink collapse reduces a
//! file's names to, so one row serves every name an inode wears. That key is
//! only as fine as the filesystem's clock, and HFS+ records mtime to the
//! second: an edit landing in the same second as the scan that read the file
//! leaves size and mtime_ns untouched, and the digest would be believed. So
//! each row also carries the time the scan that wrote it started, and a row is
//! trusted only while the file's mtime tick is strictly older than that —
//! git's "racily clean" rule. A racy row is not a disaster, only a cost: it is
//! dropped whole and the file re-read as though nothing had been remembered.
//!
//! Forging an mtime defeats the key, and that is allowed to be true, because
//! nothing here is trusted at deletion time.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use crate::consts::{CACHE_SCHEMA, CACHE_TICK_NS, CACHE_TTL_DAYS};
use crate::device::volume_uuid;
use crate::ordered::Ordered;
use crate::stages::Digest;
use crate::walk::FileRec;

/// One column per set of bytes a stage actually reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    /// The two end pages hashed together.
    Ends = 0,
    /// The rest of the head behind them.
    Deep = 1,
    /// The windows stage 4 spreads over what is left.
    Sample = 2,
    /// The whole file. Only ever learned from bytes that were compared rather
    /// than merely hashed.
    Full = 3,
}

const COLUMNS: [&str; 4] = ["ends", "deep", "sample", "full"];

struct Row {
    size: u64,
    mtime_ns: i64,
    digests: [Option<Digest>; 4],
    cached_at: i64,
}

#[derive(Default)]
struct Volume {
    database: Option<PathBuf>,
    rows: HashMap<u64, Row>,
    /// Inodes whose row changed, and inodes this scan recognised. Both are
    /// written back: a tree nobody edits would otherwise age out of its own
    /// cache and pay for a cold scan it never earned.
    touched: HashSet<u64>,
    seen: HashSet<u64>,
}

pub struct ScanCache {
    directory: PathBuf,
    volumes: HashMap<u64, Volume>,
    /// Read once, before anything is: every digest this scan stores is taken
    /// from bytes read after this moment, so a file whose mtime reaches this
    /// tick could have changed behind the read.
    cached_at: i64,
    warned: bool,
}

/// Where the per-volume databases live.
pub fn cache_home() -> PathBuf {
    if let Some(override_dir) = std::env::var_os("DEDUP_CACHE_DIR") {
        return PathBuf::from(override_dir);
    }
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
    if cfg!(target_os = "macos") {
        return home.join("Library/Caches/dedup");
    }
    std::env::var_os("XDG_CACHE_HOME")
        .map_or_else(|| home.join(".cache"), PathBuf::from)
        .join("dedup")
}

pub fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

impl ScanCache {
    pub fn new() -> Self {
        ScanCache {
            directory: cache_home(),
            volumes: HashMap::new(),
            cached_at: now_seconds(),
            warned: false,
        }
    }

    /// Say it once. An unusable cache is a slow scan, not a failed one.
    fn warn(&mut self, message: &str) {
        if !self.warned {
            self.warned = true;
            eprintln!("dedup-rs: {message}; reading everything and rebuilding it");
        }
    }

    /// Open the cache for the volume this path sits on.
    pub fn open_path(&mut self, path: &Path) {
        if let Ok(metadata) = std::fs::metadata(path) {
            use std::os::unix::fs::MetadataExt;
            self.load(metadata.dev(), path);
        }
    }

    /// ...and for any further volume the scan turned out to reach into.
    pub fn open_for(&mut self, root: &Path, records: &[FileRec]) {
        for record in records {
            if !self.volumes.contains_key(&record.dev) {
                self.load(record.dev, &root.join(record.path()));
            }
        }
    }

    fn load(&mut self, dev: u64, on_volume: &Path) {
        if self.volumes.contains_key(&dev) {
            return;
        }
        let mut volume = Volume::default();
        // The UUID survives a remount under a different name, which a mount
        // point does not. Where there is none, the mount point is all there is,
        // and guessing wrong only ever costs one cold scan.
        let name = volume_uuid(on_volume);
        let database = self.directory.join(format!("{name}.db"));
        match std::fs::create_dir_all(&self.directory)
            .map_err(|e| e.to_string())
            .and_then(|()| {
                read_rows(&database)
                    .map(|rows| volume.rows = rows)
                    .map_err(|e| e.to_string())
            }) {
            Ok(()) => volume.database = Some(database),
            Err(error) => {
                self.warn(&format!("the scan cache could not be read ({error})"));
                volume.rows.clear();
                // A file sqlite cannot make sense of is not worth repairing.
                // Clear the way so close() can lay down a fresh one instead.
                volume.database = std::fs::remove_file(&database).ok().map(|()| database);
            }
        }
        self.volumes.insert(dev, volume);
    }

    /// The remembered digest for this file, or None if there is not one.
    pub fn get(&mut self, record: &FileRec, field: Field) -> Option<Digest> {
        let volume = self.volumes.get_mut(&record.dev)?;
        let row = volume.rows.get(&record.ino)?;
        if row.size != record.size || row.mtime_ns != record.mtime_ns {
            return None;
        }
        if record.mtime_ns / CACHE_TICK_NS >= row.cached_at {
            // Racily clean: the file's clock reached the tick the writing scan
            // started in, so an edit could have slipped under the key. Every
            // column of the row was taken in that same scan and is suspect
            // together, so the row goes and the file is read as if new.
            volume.rows.remove(&record.ino);
            volume.touched.insert(record.ino);
            volume.seen.remove(&record.ino);
            return None;
        }
        volume.seen.insert(record.ino);
        volume.rows[&record.ino].digests[field as usize]
    }

    pub fn remember(&mut self, record: &FileRec, field: Field, digest: Digest) {
        let cached_at = self.cached_at;
        let Some(volume) = self.volumes.get_mut(&record.dev) else {
            return;
        };
        let row = volume.rows.entry(record.ino).or_insert_with(|| Row {
            size: record.size,
            mtime_ns: record.mtime_ns,
            digests: [None; 4],
            cached_at,
        });
        if row.size != record.size || row.mtime_ns != record.mtime_ns {
            *row = Row {
                size: record.size,
                mtime_ns: record.mtime_ns,
                digests: [None; 4],
                cached_at,
            };
        }
        // This scan read the bytes, so this scan's clock is what the row is
        // judged against next time — without this a racy row, whose key still
        // matches, would keep its old timestamp and never become trustworthy.
        row.cached_at = cached_at;
        row.digests[field as usize] = Some(digest);
        volume.touched.insert(record.ino);
    }

    /// Drop everything remembered about these files. Called when a group we
    /// vouched for turned out not to match, so the wrong answer cannot survive
    /// into the next scan.
    pub fn forget(&mut self, records: &[&FileRec]) {
        for record in records {
            if let Some(volume) = self.volumes.get_mut(&record.dev) {
                if volume.rows.remove(&record.ino).is_some() {
                    volume.touched.insert(record.ino);
                    volume.seen.remove(&record.ino);
                }
            }
        }
    }

    /// Split stage-5 candidates into (to compare, matched from memory).
    ///
    /// A group only goes the fast way when every member's whole-file digest is
    /// remembered. One unknown member and the whole group is read as usual,
    /// which re-learns the others on the way through.
    pub fn attested_runs(
        &mut self,
        records: &[FileRec],
        groups: Vec<Vec<usize>>,
    ) -> (Vec<Vec<usize>>, Vec<Vec<usize>>) {
        let mut pending = Vec::new();
        let mut matched = Vec::new();
        for members in groups {
            let digests: Vec<Option<Digest>> = members
                .iter()
                .map(|&index| self.get(&records[index], Field::Full))
                .collect();
            if digests.iter().any(Option::is_none) {
                pending.push(members);
                continue;
            }
            let mut by_digest: Ordered<Digest, usize> = Ordered::new();
            for (&index, digest) in members.iter().zip(digests) {
                by_digest.push(digest.expect("checked above"), index);
            }
            matched.extend(
                by_digest
                    .into_values()
                    .into_iter()
                    .filter(|run| run.len() > 1),
            );
        }
        (pending, matched)
    }

    /// Write back what changed and drop what has not been seen in months.
    pub fn close(&mut self) {
        let now = now_seconds();
        let cutoff = now - CACHE_TTL_DAYS * 86_400;
        let mut failure = None;
        for volume in self.volumes.values() {
            let Some(database) = &volume.database else {
                continue;
            };
            let writing: Vec<u64> = volume.touched.union(&volume.seen).copied().collect();
            if let Err(error) = write_rows(database, volume, &writing, now, cutoff) {
                failure = Some(error.to_string());
            }
        }
        if let Some(error) = failure {
            self.warn(&format!("the scan cache could not be written ({error})"));
        }
        self.volumes.clear();
    }
}

fn prepare(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS files (
             ino INTEGER PRIMARY KEY, size INTEGER NOT NULL,
             mtime_ns INTEGER NOT NULL, ends BLOB, deep BLOB, sample BLOB,
             full BLOB, cached_at INTEGER NOT NULL, last_seen INTEGER NOT NULL);
         PRAGMA user_version = {CACHE_SCHEMA};"
    ))
}

fn read_rows(database: &Path) -> rusqlite::Result<HashMap<u64, Row>> {
    let connection = Connection::open(database)?;
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version != CACHE_SCHEMA {
        // Another version's columns do not mean what this one thinks they
        // mean. Start over rather than interpret them.
        connection.execute_batch("DROP TABLE IF EXISTS files")?;
        prepare(&connection)?;
        return Ok(HashMap::new());
    }
    prepare(&connection)?;
    let mut statement = connection
        .prepare("SELECT ino, size, mtime_ns, ends, deep, sample, full, cached_at FROM files")?;
    let mut rows = HashMap::new();
    let mut cursor = statement.query([])?;
    while let Some(row) = cursor.next()? {
        let mut digests = [None; 4];
        for (slot, digest) in digests.iter_mut().enumerate() {
            *digest = row
                .get::<_, Option<Vec<u8>>>(3 + slot)?
                .and_then(|bytes| Digest::try_from(bytes.as_slice()).ok());
        }
        rows.insert(
            row.get::<_, i64>(0)? as u64,
            Row {
                size: row.get::<_, i64>(1)? as u64,
                mtime_ns: row.get(2)?,
                digests,
                cached_at: row.get(7)?,
            },
        );
    }
    Ok(rows)
}

fn write_rows(
    database: &Path,
    volume: &Volume,
    writing: &[u64],
    now: i64,
    cutoff: i64,
) -> rusqlite::Result<()> {
    let mut connection = Connection::open(database)?;
    prepare(&connection)?;
    let transaction = connection.transaction()?;
    {
        let mut insert = transaction.prepare(&format!(
            "INSERT OR REPLACE INTO files (ino, size, mtime_ns, {}, cached_at, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            COLUMNS.join(", ")
        ))?;
        let mut delete = transaction.prepare("DELETE FROM files WHERE ino = ?1")?;
        for &ino in writing {
            match volume.rows.get(&ino) {
                None => {
                    delete.execute([ino as i64])?;
                }
                Some(row) => {
                    insert.execute(rusqlite::params![
                        ino as i64,
                        row.size as i64,
                        row.mtime_ns,
                        row.digests[0].map(|d| d.to_vec()),
                        row.digests[1].map(|d| d.to_vec()),
                        row.digests[2].map(|d| d.to_vec()),
                        row.digests[3].map(|d| d.to_vec()),
                        row.cached_at,
                        now,
                    ])?;
                }
            }
        }
        transaction.execute("DELETE FROM files WHERE last_seen < ?1", [cutoff])?;
    }
    transaction.commit()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(ino: u64, size: u64, mtime_ns: i64) -> FileRec {
        FileRec {
            names: vec![format!("f{ino}.bin")],
            size,
            dev: 1,
            ino,
            nlink: 1,
            mtime_ns,
            retained: None,
        }
    }

    fn cache_with(cached_at: i64) -> ScanCache {
        let mut cache = ScanCache {
            directory: PathBuf::from("/nonexistent"),
            volumes: HashMap::new(),
            cached_at,
            warned: false,
        };
        cache.volumes.insert(1, Volume::default());
        cache
    }

    #[test]
    fn a_row_whose_key_still_matches_is_returned() {
        let mut cache = cache_with(100);
        let file = record(7, 10, 50 * CACHE_TICK_NS);
        cache.remember(&file, Field::Ends, [9; 32]);
        assert_eq!(cache.get(&file, Field::Ends), Some([9; 32]));
        assert_eq!(cache.get(&file, Field::Full), None);
    }

    #[test]
    fn an_edited_file_misses_on_size_or_mtime() {
        let mut cache = cache_with(100);
        cache.remember(&record(7, 10, 50 * CACHE_TICK_NS), Field::Ends, [9; 32]);
        assert_eq!(
            cache.get(&record(7, 11, 50 * CACHE_TICK_NS), Field::Ends),
            None
        );
        assert_eq!(
            cache.get(&record(7, 10, 51 * CACHE_TICK_NS), Field::Ends),
            None
        );
    }

    #[test]
    fn a_racily_clean_row_is_dropped_whole_rather_than_re_stamped() {
        // The file's mtime second is not strictly older than the second the
        // row was written, so nothing in the row may be believed — including
        // the columns a different stage filled.
        let mut cache = cache_with(100);
        let file = record(7, 10, 100 * CACHE_TICK_NS);
        cache.remember(&file, Field::Ends, [1; 32]);
        cache.remember(&file, Field::Full, [2; 32]);
        assert_eq!(cache.get(&file, Field::Ends), None);
        assert_eq!(cache.get(&file, Field::Full), None);
        assert!(!cache.volumes[&1].rows.contains_key(&7));
        assert!(cache.volumes[&1].touched.contains(&7));
    }

    #[test]
    fn a_row_one_tick_older_than_the_scan_is_trusted() {
        let mut cache = cache_with(100);
        let file = record(7, 10, 99 * CACHE_TICK_NS + 999_999_999);
        cache.remember(&file, Field::Ends, [3; 32]);
        assert_eq!(cache.get(&file, Field::Ends), Some([3; 32]));
    }

    #[test]
    fn one_member_without_a_full_digest_sends_the_whole_group_back_to_be_read() {
        let mut cache = cache_with(100);
        let records = vec![
            record(1, 10, 50 * CACHE_TICK_NS),
            record(2, 10, 50 * CACHE_TICK_NS),
            record(3, 10, 50 * CACHE_TICK_NS),
        ];
        cache.remember(&records[0], Field::Full, [4; 32]);
        cache.remember(&records[1], Field::Full, [4; 32]);
        let (pending, matched) = cache.attested_runs(&records, vec![vec![0, 1, 2]]);
        assert_eq!(pending, [vec![0, 1, 2]]);
        assert!(matched.is_empty());

        cache.remember(&records[2], Field::Full, [5; 32]);
        let (pending, matched) = cache.attested_runs(&records, vec![vec![0, 1, 2]]);
        assert!(pending.is_empty());
        assert_eq!(matched, [vec![0, 1]]);
    }

    #[test]
    fn forgetting_removes_the_row_and_marks_it_for_deletion() {
        let mut cache = cache_with(100);
        let file = record(7, 10, 50 * CACHE_TICK_NS);
        cache.remember(&file, Field::Full, [6; 32]);
        cache.forget(&[&file]);
        assert_eq!(cache.get(&file, Field::Full), None);
        assert!(cache.volumes[&1].touched.contains(&7));
    }
}
