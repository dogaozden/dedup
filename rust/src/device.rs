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
use std::process::Command;
use std::thread::JoinHandle;

#[derive(Clone, Copy, Debug)]
pub struct Device {
    /// Some(true) flash, Some(false) spinning, None when it cannot be told.
    /// USB enclosures, network shares and disk images generally do not say.
    pub solid: Option<bool>,
    requested: Option<usize>,
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
    pub fn random_jobs(&self) -> usize {
        match self.requested {
            Some(n) => n.max(1),
            // An unknown device gets the generous pool on purpose: it is
            // usually an enclosure that simply does not report, and treating
            // one of those as a spinning disk costs far more than the reverse.
            None if self.solid == Some(false) => 2,
            None => cpu_bound(8),
        }
    }

    /// Readers for the stage that streams whole files in step.
    pub fn sequential_jobs(&self) -> usize {
        match self.requested {
            Some(n) => n.max(1),
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
    match ask_diskutil(&mount_point(path), "SolidState")?.as_str() {
        value if value.starts_with("<true") => Some(true),
        value if value.starts_with("<false") => Some(false),
        _ => None,
    }
}

/// A stable filename for the cache belonging to the volume `path` is on.
///
/// The UUID survives a remount under a different name, which a mount point
/// does not. Where there is no UUID to be had, the mount point is all there is,
/// and guessing wrong only ever costs one cold scan.
pub fn volume_uuid(path: &Path) -> String {
    let root = mount_point(path);
    if let Some(uuid) = ask_diskutil(&root, "VolumeUUID")
        .as_deref()
        .and_then(|value| value.strip_prefix("<string>"))
        .and_then(|value| value.split('<').next())
        .filter(|value| !value.is_empty())
    {
        return uuid.to_string();
    }
    let digest = crate::stages::sha256(root.as_os_str().as_encoded_bytes());
    digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()[..32]
        .to_string()
}

fn ask_diskutil(root: &Path, key: &str) -> Option<String> {
    let output = Command::new("diskutil")
        .args(["info", "-plist"])
        .arg(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    plist_value(&text, key).map(ToString::to_string)
}

/// Whatever element follows `<key>name</key>`. Enough of a plist reader for
/// two boolean-or-string lookups, and it keeps the crate list short.
fn plist_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let marker = format!("<key>{key}</key>");
    let rest = text.split_once(&marker)?.1;
    Some(rest.trim_start())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_lookup_finds_the_element_after_the_key() {
        let text = "<dict>\n\t<key>SolidState</key>\n\t<true/>\n\t<key>VolumeUUID</key>\n\t<string>ABC-123</string>\n</dict>";
        assert!(plist_value(text, "SolidState")
            .expect("key present")
            .starts_with("<true"));
        assert!(plist_value(text, "VolumeUUID")
            .expect("key present")
            .starts_with("<string>ABC-123<"));
        assert!(plist_value(text, "Missing").is_none());
    }

    #[test]
    fn an_explicit_j_overrides_both_pools() {
        let device = Device {
            solid: Some(false),
            requested: Some(8),
        };
        assert_eq!(device.random_jobs(), 8);
        assert_eq!(device.sequential_jobs(), 8);
    }

    #[test]
    fn an_unknown_device_gets_a_wide_random_pool_and_a_serial_stream() {
        let device = Device {
            solid: None,
            requested: None,
        };
        assert!(device.random_jobs() > 1);
        assert_eq!(device.sequential_jobs(), 1);
        assert!(!device.all_solid_state());
    }

    #[test]
    fn the_mount_point_of_the_root_is_the_root() {
        assert_eq!(mount_point(Path::new("/")), PathBuf::from("/"));
    }
}
