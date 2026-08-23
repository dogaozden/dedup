//! What kind of disk this is, and how many readers it wants.
//!
//! The oracle collapses this to one reader count. REWRITE.md §4 asks the port
//! not to: a stage that seeks (the digest probes) and a stage that streams
//! (the byte comparison) want different queue depths, and the asymmetry is the
//! point — guessing wrong about random reads on flash costs multiples, while
//! guessing wrong on a spinning disk costs about thirty percent. An explicit
//! `-j` overrides everything, which is what keeps this identical to the oracle
//! wherever the oracle has an opinion.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;

#[derive(Clone, Copy, Debug)]
pub struct Device {
    /// Some(true) flash, Some(false) spinning, None when it cannot be told.
    /// USB enclosures, network shares and disk images generally do not say.
    pub solid: Option<bool>,
    requested: Option<usize>,
}

/// Which access patterns the scan is being asked to optimise for.
///
/// Not the same question as `Device::solid`, which is what the disk says about
/// itself. A user with a USB enclosure that reports nothing knows what is
/// inside it and we do not, so the two are kept apart: the device answers, the
/// profile decides.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Profile {
    /// Every stage as it has always been. This is what the conformance gate
    /// grades and the only thing RBENCH scores.
    #[default]
    Flash,
    /// Fewer, longer, more nearly sequential reads, at the cost of reading
    /// more bytes. See SPIN.md.
    Spinning,
}

/// What `--profile` asked for, before the disk has answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Wanted {
    #[default]
    Auto,
    Flash,
    Spinning,
}

/// Said once, on stderr, when `auto` had nothing to go on.
pub const UNKNOWN_MEDIUM: &str =
    "dedup-rs: this volume does not say whether it spins, so the scan is reading it as flash. \
     If it is a USB hard disk, --profile spinning will read it in far fewer seeks.";

impl Wanted {
    pub fn parse(text: &str) -> Option<Wanted> {
        match text {
            "auto" => Some(Wanted::Auto),
            "flash" => Some(Wanted::Flash),
            "spinning" => Some(Wanted::Spinning),
            _ => None,
        }
    }

    /// The profile to scan with, and the hint to print if there is one.
    ///
    /// `auto` picks `spinning` only for a disk that positively said it spins.
    /// Solid state and "no idea" both get `flash`, which is what the tool has
    /// always done and what keeps a default run byte-identical to the oracle.
    /// Guessing `spinning` at an enclosure that simply does not report would
    /// cost every ordinary external SSD a slower scan.
    pub fn resolve(self, device: Device) -> (Profile, Option<&'static str>) {
        match self {
            Wanted::Flash => (Profile::Flash, None),
            Wanted::Spinning => (Profile::Spinning, None),
            Wanted::Auto => match device.solid {
                Some(false) => (Profile::Spinning, None),
                Some(true) => (Profile::Flash, None),
                None => (Profile::Flash, Some(UNKNOWN_MEDIUM)),
            },
        }
    }
}

/// A volume probe in flight.
///
/// Asking what kind of disk this is means running `diskutil`, and a subprocess
/// costs more than the whole scan of a small tree does. Nothing before the
/// reader pools are sized needs the answer, so the ask goes out on a thread as
/// soon as the root is known and is collected where it is first wanted — by
/// which time the walk has been running against it.
pub struct VolumeProbe {
    asking: Option<JoinHandle<Option<bool>>>,
    solid: Option<bool>,
    requested: Option<usize>,
}

impl VolumeProbe {
    pub fn start(root: &Path, requested: Option<usize>) -> Self {
        let volume = root.to_path_buf();
        VolumeProbe {
            asking: Some(std::thread::spawn(move || is_solid_state(&volume))),
            solid: None,
            requested,
        }
    }

    /// The answer, waited for the first time and free afterwards.
    pub fn device(&mut self) -> Device {
        if let Some(asking) = self.asking.take() {
            // A probe thread that panicked leaves us where an unreadable
            // volume would: not knowing, which every caller already handles.
            self.solid = asking.join().unwrap_or(None);
        }
        Device {
            solid: self.solid,
            requested: self.requested,
        }
    }
}

#[cfg(test)]
impl VolumeProbe {
    /// A probe that has already answered.
    ///
    /// The only way to exercise `auto` against media this machine does not
    /// have: nothing here reports rotational, and the tool's whole point is
    /// what it does when something does.
    pub fn answered(solid: Option<bool>, requested: Option<usize>) -> Self {
        VolumeProbe {
            asking: None,
            solid,
            requested,
        }
    }
}

impl Drop for VolumeProbe {
    /// A scan that never needed the answer still waits for it here rather than
    /// exiting over the top of a live subprocess: an orphan finishing after we
    /// are gone is load on whatever runs next.
    fn drop(&mut self) {
        if let Some(asking) = self.asking.take() {
            let _ = asking.join();
        }
    }
}

impl Device {
    /// True only when the disk is known to be flash. "Don't know" is a no —
    /// this is what picks the comparison chunk, and a spinning disk pays a
    /// seek every time the round moves to the next file in a group.
    pub fn all_solid_state(&self) -> bool {
        self.solid == Some(true)
    }

    /// Readers for the stages that seek: the end probe, the deep head, the
    /// sample windows, and the small-file passes.
    pub fn random_jobs(&self, profile: Profile) -> usize {
        match self.requested {
            Some(n) => n.max(1),
            // One set of heads, one queue. Two readers on a spinning disk
            // spend their time interleaving each other's tracks.
            None if profile == Profile::Spinning => 1,
            // An unknown device gets the generous pool on purpose: it is
            // usually an enclosure that simply does not report, and treating
            // one of those as a spinning disk costs far more than the reverse.
            None if self.solid == Some(false) => 2,
            // These stages open a file, read a page or two of it and close it
            // again, which is `fast_bound`'s case and not `cpu_bound`'s.
            None => fast_bound(8),
        }
    }

    /// Readers for the stage that settles small files in one pass.
    ///
    /// That stage opens a file, reads the whole of it and moves on, so it is
    /// waiting on the filesystem rather than computing, and every reader is
    /// asking the same catalog. Past a reader per performance core the extra
    /// ones queue rather than add throughput, and an efficiency core waits at
    /// a lower clock. The probes below read a page out of a much larger file
    /// and do not behave this way, which is why this is its own number and
    /// not a narrowing of `random_jobs`.
    pub fn small_file_jobs(&self, profile: Profile) -> usize {
        match self.requested {
            Some(n) => n.max(1),
            None if profile == Profile::Spinning => 1,
            None if self.solid == Some(false) => 2,
            None => fast_bound(8),
        }
    }

    /// Readers for the stage that streams whole files in step.
    pub fn sequential_jobs(&self, profile: Profile) -> usize {
        match self.requested {
            Some(n) => n.max(1),
            None if profile == Profile::Spinning => 1,
            // Serial where the heads would have to seek between files, and
            // where we cannot be sure they would not.
            None if self.solid == Some(true) => cpu_bound(8),
            None => 1,
        }
    }
}

fn cpu_bound(ceiling: usize) -> usize {
    ceiling.min(std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get))
}

/// The bound for the stages that open a file, read a little of it and move on.
///
/// Those stages are waiting on the filesystem, not computing, and past a
/// point more readers only queue deeper: the catalog they are all asking is
/// one structure. Measured on this machine, the small-file stage is fastest
/// with a reader per performance core and gets steadily slower with more —
/// an efficiency core waits for the same catalog at a lower clock, and the
/// extra readers are contention rather than throughput.
///
/// The head, deep-head and sample probes have the same shape — one open, a
/// page or two, close — and measure the same way over `widegroup`'s 1,827
/// files, best of nine:
///
/// ```text
///   readers        ends    deep head     compare (512 KB)
///        1      17.91 ms    21.56 ms         5.01 ms
///        2      10.99       12.24            3.82
///        4       6.38        7.40            3.44
///        8      10.25       10.54            2.79
///       12      11.06       11.03            -
/// ```
///
/// Four is 38% better than eight for the probes and 19% worse for the
/// comparison, which is the whole distinction: the stage that streams whole
/// files wants the cores and keeps `cpu_bound`; the stages that ask the
/// catalog want the fast ones and no more.
///
/// Where the cores are all alike this is exactly `cpu_bound`.
pub fn fast_bound(ceiling: usize) -> usize {
    let cores = crate::sys::performance_cores().unwrap_or_else(|| {
        std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get)
    });
    ceiling.min(cores.max(1))
}

/// The volume root `path` sits on — the only thing diskutil accepts.
pub fn mount_point(path: &Path) -> PathBuf {
    let resolved = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let Ok(dev) = fs::metadata(&resolved).map(|m| m.dev()) else {
        return PathBuf::from("/");
    };
    let mut current = resolved.as_path();
    while let Some(parent) = current.parent() {
        match fs::metadata(parent) {
            Ok(metadata) if metadata.dev() == dev => current = parent,
            _ => return current.to_path_buf(),
        }
    }
    current.to_path_buf()
}

fn is_solid_state(path: &Path) -> Option<bool> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    if let Some(solid) = crate::sys::solid_state(path) {
        return Some(solid);
    }
    // The registry will not call a disk image flash, because it is not flash.
    // But every caller of this is asking one question — does this volume
    // charge for a seek — and a disk image does not: it has no heads, and
    // what it sits on is somebody else's problem. Answering `None` here lumps
    // it in with the USB enclosure that did not report, which is a device that
    // may genuinely spin, and costs it the parallel reader for nothing.
    crate::sys::disk_image(path).then_some(true)
}

/// A stable filename for the cache belonging to the volume `path` is on.
///
/// The UUID survives a remount under a different name, which a mount point
/// does not. Where there is no UUID to be had, the mount point is all there is,
/// and guessing wrong only ever costs one cold scan.
pub fn volume_uuid(path: &Path) -> String {
    let root = mount_point(path);
    if let Some(uuid) = crate::sys::volume_uuid(&root) {
        return uuid;
    }
    let digest = crate::stages::sha256(root.as_os_str().as_encoded_bytes());
    digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()[..32]
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_j_overrides_both_pools_in_either_profile() {
        let device = Device {
            solid: Some(false),
            requested: Some(8),
        };
        assert_eq!(device.random_jobs(Profile::Flash), 8);
        assert_eq!(device.sequential_jobs(Profile::Flash), 8);
        assert_eq!(device.random_jobs(Profile::Spinning), 8);
        assert_eq!(device.sequential_jobs(Profile::Spinning), 8);
    }

    #[test]
    fn an_unknown_device_gets_a_wide_random_pool_and_a_serial_stream() {
        let device = Device {
            solid: None,
            requested: None,
        };
        assert!(device.random_jobs(Profile::Flash) > 1);
        assert_eq!(device.sequential_jobs(Profile::Flash), 1);
        assert!(!device.all_solid_state());
    }

    #[test]
    fn the_spinning_profile_reads_with_one_thread_even_on_flash() {
        // Forced onto a disk that says it is solid state: the profile is what
        // decides, or forcing it would not be worth having.
        let device = Device {
            solid: Some(true),
            requested: None,
        };
        assert_eq!(device.random_jobs(Profile::Spinning), 1);
        assert_eq!(device.sequential_jobs(Profile::Spinning), 1);
        assert!(device.random_jobs(Profile::Flash) > 1);
    }

    #[test]
    fn auto_only_picks_spinning_for_a_disk_that_said_it_spins() {
        let of = |solid| Device {
            solid,
            requested: None,
        };
        assert_eq!(
            Wanted::Auto.resolve(of(Some(false))),
            (Profile::Spinning, None)
        );
        assert_eq!(Wanted::Auto.resolve(of(Some(true))), (Profile::Flash, None));
        let (profile, hint) = Wanted::Auto.resolve(of(None));
        assert_eq!(profile, Profile::Flash);
        assert_eq!(hint, Some(UNKNOWN_MEDIUM));
    }

    #[test]
    fn an_explicit_profile_never_asks_the_disk_and_never_hints() {
        let of = |solid| Device {
            solid,
            requested: None,
        };
        for solid in [Some(true), Some(false), None] {
            assert_eq!(Wanted::Flash.resolve(of(solid)), (Profile::Flash, None));
            assert_eq!(
                Wanted::Spinning.resolve(of(solid)),
                (Profile::Spinning, None)
            );
        }
    }

    #[test]
    fn the_profile_names_are_the_three_the_flag_documents() {
        assert_eq!(Wanted::parse("auto"), Some(Wanted::Auto));
        assert_eq!(Wanted::parse("flash"), Some(Wanted::Flash));
        assert_eq!(Wanted::parse("spinning"), Some(Wanted::Spinning));
        assert_eq!(Wanted::parse("Spinning"), None);
        assert_eq!(Wanted::parse("hdd"), None);
    }

    #[test]
    fn the_mount_point_of_the_root_is_the_root() {
        assert_eq!(mount_point(Path::new("/")), PathBuf::from("/"));
    }
}

#[cfg(test)]
mod pools {
    use super::*;

    /// The small-file stage gets a reader per performance core; the probes
    /// and the streaming compare get the wider pool. On a machine whose cores
    /// are all alike the two are the same number, which is why this compares
    /// them to the bounds rather than to a constant.
    #[test]
    fn the_small_file_pool_is_sized_from_a_different_count() {
        let device = Device {
            solid: Some(true),
            requested: None,
        };
        assert_eq!(device.small_file_jobs(Profile::Flash), fast_bound(8));
        assert_eq!(device.random_jobs(Profile::Flash), fast_bound(8));
        assert_eq!(device.sequential_jobs(Profile::Flash), cpu_bound(8));
        assert!(device.small_file_jobs(Profile::Flash) <= device.random_jobs(Profile::Flash));
    }

    /// An explicit -j still overrides every pool, the new one included.
    #[test]
    fn an_explicit_j_overrides_the_small_file_pool_too() {
        let device = Device {
            solid: None,
            requested: Some(3),
        };
        assert_eq!(device.small_file_jobs(Profile::Flash), 3);
        assert_eq!(device.small_file_jobs(Profile::Spinning), 3);
    }

    /// A disk that says it spins gets one reader there as everywhere else.
    #[test]
    fn a_spinning_disk_settles_small_files_with_one_reader() {
        let device = Device {
            solid: Some(false),
            requested: None,
        };
        assert_eq!(device.small_file_jobs(Profile::Spinning), 1);
    }

    /// Neither bound may ever hand back a pool of nothing.
    #[test]
    fn every_pool_has_at_least_one_reader() {
        assert!(fast_bound(8) >= 1);
        assert!(cpu_bound(8) >= 1);
    }
}
