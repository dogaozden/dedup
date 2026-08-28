//! Cloud placeholders: the files a provider keeps a name for and not the bytes.
//!
//! macOS marks these `SF_DATALESS`. Reading one byte of a dataless file makes
//! the provider fetch the whole thing over the network, which is why the
//! maintainer's scan of a home directory sat at 8 KiB/s on an SSD: it had
//! wandered into `~/Library/CloudStorage/GoogleDrive-…` and put a 470 MB `.MOV`
//! with 12 MB actually on disk at the head of the queue.
//!
//! **The law this file exists to keep:** a default scan never causes a single
//! byte of materialization. Not from the walk, not from sampling, not from a
//! proof at apply.
//!
//! Two independent mechanisms, because one of them is a test and the other is
//! a guarantee:
//!
//! * The walk reads `ATTR_CMN_FLAGS` in the bulk request it already makes and
//!   drops a dataless entry before it becomes a record. This is the one that
//!   keeps the count honest and the scan fast.
//! * The process then asks the kernel to fail rather than materialize
//!   (`IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES`). This is the one that holds
//!   when the first fails — a file evicted to the cloud *after* the walk saw it
//!   with its bytes present cannot be caught by any amount of walk-time care,
//!   and without this policy reading it would start a download.
//!
//! The allocated-vs-logical-size heuristic is deliberately NOT the detector.
//! Sparse files and `UF_COMPRESSED` files legitimately hold fewer blocks than
//! their length, and calling those cloud placeholders would silently drop real
//! local duplicates out of the scan.

#[cfg(test)]
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// `SF_DATALESS` from `<sys/stat.h>`: "file is dataless object".
///
/// One of the `SF_SYNTHETIC` flags, which the header keeps out of
/// `SF_SUPPORTED` precisely so that no userland process can set or clear it.
/// That is what makes it trustworthy: when it is on, the kernel put it on.
pub const SF_DATALESS: u32 = 0x4000_0000;

/// The detection seam. Everything above this is a policy decision; this is the
/// whole of the fact.
///
/// Taking the flags word rather than a path is what makes the rule testable
/// without a real cloud file, which cannot be fabricated: the walk hands over
/// what `getattrlistbulk` reported and a test hands over whatever it likes.
pub fn dataless(flags: u32) -> bool {
    flags & SF_DATALESS != 0
}

// ---------------------------------------------------------------------------
// The I/O policy — defence in depth.
// ---------------------------------------------------------------------------

/// `IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES` from `<sys/resource.h>`.
///
/// 3, not 6. The neighbouring `IOPOL_TYPE_VFS_*` constants are easy to misread
/// from memory; this one was taken from the SDK header on the machine that
/// builds this.
const IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: libc::c_int = 3;
const IOPOL_SCOPE_PROCESS: libc::c_int = 0;
const IOPOL_MATERIALIZE_DATALESS_FILES_OFF: libc::c_int = 1;
/// `getiopolicy_np` may return the basic value with `…_ORIG` (4) set beside it,
/// so the answer is masked before it is compared.
const IOPOL_MATERIALIZE_DATALESS_FILES_BASIC_MASK: libc::c_int = 3;

extern "C" {
    fn setiopolicy_np(iotype: libc::c_int, scope: libc::c_int, policy: libc::c_int) -> libc::c_int;
    fn getiopolicy_np(iotype: libc::c_int, scope: libc::c_int) -> libc::c_int;
}

/// Ask the kernel to fail reads of dataless files rather than fetch them, and
/// answer whether it is now really doing so.
///
/// Process-scoped, so it covers every reader thread including ones rayon has
/// not built yet.
///
/// The answer comes from `getiopolicy_np` rather than from `setiopolicy_np`'s
/// return, because those are different claims: one is "the call did not fail"
/// and the other is "the policy is in force", and only the second is the thing
/// the law rests on. An older or unusual system that will not take the policy
/// is not a reason to refuse to scan — it is a reason for the walk-time
/// exclusion to be the only guard, which is what the summary then says.
pub fn refuse_materialization() -> bool {
    // SAFETY: a plain three-integer libc call with no pointers involved.
    unsafe {
        setiopolicy_np(
            IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
            IOPOL_SCOPE_PROCESS,
            IOPOL_MATERIALIZE_DATALESS_FILES_OFF,
        );
    }
    materialization_refused() == Some(true)
}

/// Install the refusal, unless this run asked for the downloads.
///
/// The decision and the syscall in one place, so the decision is testable:
/// `main` calls exactly this, and a test can ask it both questions. `--cloud`
/// does not LIFT an installed policy, it declines to install one — a run that
/// asked to compare cloud files has asked for the downloads, and overriding a
/// policy the surrounding environment set for its own reasons is not this
/// tool's business.
///
/// Answers whether the kernel is now refusing on this process's behalf.
pub fn install_refusal(wants_cloud: bool) -> bool {
    !wants_cloud && refuse_materialization()
}

/// What the policy is right now, asked of the kernel rather than remembered.
///
/// A flag this file set and then reported back would be proving that a bool
/// stayed true. This asks the process's real policy, which is the thing the
/// law depends on. `None` where the platform will not answer.
pub fn materialization_refused() -> Option<bool> {
    // SAFETY: a plain two-integer libc call with no pointers involved.
    let policy = unsafe {
        getiopolicy_np(
            IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
            IOPOL_SCOPE_PROCESS,
        )
    };
    if policy < 0 {
        return None;
    }
    Some(
        policy & IOPOL_MATERIALIZE_DATALESS_FILES_BASIC_MASK
            == IOPOL_MATERIALIZE_DATALESS_FILES_OFF,
    )
}

/// The flags word of one path, without following a symlink and without opening
/// it. `None` where the file is gone or will not answer.
///
/// `lstat` and not `stat`: the walk never follows a symlink, so neither does
/// the question asked about what it found.
pub fn flags_of(path: &Path) -> Option<u32> {
    use std::os::macos::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(path).ok()?;
    Some(metadata.st_flags())
}

// ---------------------------------------------------------------------------
// What a run has to say about it afterwards.
// ---------------------------------------------------------------------------

/// Which of the files that could not be read live in the cloud.
///
/// This is how a file EVICTED MID-SCAN becomes legible. The walk saw it with
/// its bytes present and let it through; by the time a stage tried to read it
/// the provider had taken the bytes back, the I/O policy turned that read into
/// an `EDEADLK` rather than a download, and the existing read-error path did
/// what it does with any unreadable file — dropped it from its group and named
/// it here. All that is left is to say *why*, and the honest way to say why is
/// to ask the file, which is what `flags_of` does.
///
/// Asked of the flag rather than of the errno on purpose. `EDEADLK` is how the
/// refusal arrives today; `SF_DATALESS` is what is actually true about the
/// file, it is the same fact the walk filters on, and it stays right if the
/// read failed at a different call or came back with a different number.
///
/// `flags` is a parameter so this can be tested against files that are not
/// there to be made: production passes `flags_of`.
pub fn in_cloud<F>(unreadable: &[String], flags: F) -> Vec<String>
where
    F: Fn(&str) -> Option<u32>,
{
    unreadable
        .iter()
        .filter(|name| flags(name).is_some_and(dataless))
        .cloned()
        .collect()
}

/// One line for a person, or nothing at all when nothing was skipped.
///
/// Nothing is said on a run that met no cloud file, which is what keeps every
/// output on a dataless-free tree byte-identical to the build before this one.
pub fn notice(skipped: u64) -> Option<String> {
    (skipped > 0).then(|| {
        format!(
            "{skipped} file{} in cloud — not scanned (--cloud to include)",
            if skipped == 1 { "" } else { "s" }
        )
    })
}

/// A note naming the files that went to the cloud while the scan was reading
/// them, for the run that met one.
pub fn evicted_notice(names: &[String]) -> Option<String> {
    let (first, rest) = names.split_first()?;
    let more = match rest.len() {
        0 => String::new(),
        1 => " (and 1 more)".to_string(),
        many => format!(" (and {many} more)"),
    };
    Some(format!(
        "dedup-rs: {first} lives in the cloud and was evicted while this scan \
         was reading it{more}; it was left out of its group rather than \
         downloaded. --cloud to download and compare."
    ))
}

/// What one scan met. Shared the way `Effort` is shared, because the browser
/// has to be able to say it while the walk is still running.
#[derive(Default)]
pub struct Cloud {
    skipped: AtomicU64,
    /// Named rather than counted: a person who is told a file was left out can
    /// only act on it if they are told which one.
    evicted: Mutex<Vec<String>>,
}

impl Cloud {
    /// One dataless file the walk refused to make a record of.
    #[inline]
    pub fn skip(&self) {
        self.skipped.fetch_add(1, Ordering::Relaxed);
    }

    pub fn skipped(&self) -> u64 {
        self.skipped.load(Ordering::Relaxed)
    }

    /// The files that failed their read and turned out to be dataless. Added
    /// to the skipped count, because from a user's point of view they are the
    /// same thing: a file in the cloud that this scan did not read.
    pub fn evicted(&self, names: Vec<String>) {
        if names.is_empty() {
            return;
        }
        self.skipped
            .fetch_add(names.len() as u64, Ordering::Relaxed);
        self.evicted
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .extend(names);
    }

    pub fn evicted_names(&self) -> Vec<String> {
        self.evicted
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .clone()
    }
}

/// Whether this error is the kernel refusing to fetch a dataless file.
///
/// `#[cfg(test)]` on purpose, and worth saying why rather than leaving a
/// reader to wonder: the scan does NOT branch on this, and must not. The
/// read-error path treats a refused materialization exactly as it treats a
/// permission failure or a vanished file — the file leaves its group and is
/// named — and that uniformity is the reason the graceful degradation needed
/// no new code at all. This exists so a test can build the exact error the
/// policy produces and prove that claim instead of asserting it.
#[cfg(test)]
pub fn refused_materialization(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::EDEADLK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dataless_flag_is_the_detector_and_nothing_else_is() {
        // The flags a real file carries, and the one that decides.
        assert!(dataless(SF_DATALESS));
        assert!(dataless(SF_DATALESS | 0x0000_0020)); // ...and compressed too
        assert!(!dataless(0));
        // The near misses, spelled out because getting one of these wrong is
        // how a scan starts skipping files it should read. UF_COMPRESSED is a
        // file whose bytes ARE here, held in an extended attribute; a sparse
        // file has no flag at all. Both under-allocate, and neither is cloud.
        assert!(!dataless(0x0000_0020), "UF_COMPRESSED is not dataless");
        assert!(!dataless(0x0000_0002), "UF_IMMUTABLE is not dataless");
        assert!(!dataless(0x0080_0000), "SF_FIRMLINK is not dataless");
        assert!(!dataless(0x0002_0000), "SF_IMMUTABLE is not dataless");
        // SF_DATALESS is the top half of SF_SYNTHETIC; its neighbour is not it.
        assert!(!dataless(0x8000_0000), "the other synthetic bit is not it");
        assert_eq!(SF_DATALESS, 0x4000_0000, "the value in <sys/stat.h>");
    }

    #[test]
    fn the_kernel_is_asked_to_refuse_and_then_asked_whether_it_did() {
        // In-process, because a policy is a property of this process and the
        // only honest way to check it is to ask this process's kernel. The
        // scan sets it from `main`; a test binary has to set it itself.
        //
        // Process-scoped state in a threaded test runner: this is set here and
        // never unset, which is safe because no other test reads a file's
        // contents through a path that could be dataless — and safer than the
        // alternative, since a test that turned materialization back ON would
        // be a test that can start a download.
        let before = materialization_refused();
        assert!(
            before.is_some(),
            "this platform would not say what its policy is"
        );
        assert!(refuse_materialization(), "the kernel refused the policy");
        assert_eq!(
            materialization_refused(),
            Some(true),
            "the policy was set and the kernel does not agree it is set"
        );
    }

    #[test]
    fn the_policy_is_installed_by_default_and_declined_under_cloud() {
        // The decision `main` makes, asked directly. The test above proves the
        // syscall pair works; this one proves the flag is read the right way
        // round, which is a different thing and the one a tidy-up would break.
        //
        // What NEITHER test reaches is `main`'s single call site. A process's
        // I/O policy is not observable from outside that process, and no test
        // here can make a dataless file to observe it indirectly, so deleting
        // that one line would leave every test green. `tools/cloud-validate.sh`
        // is what closes it, on a real cloud folder, on the maintainer's own
        // machine. Said here rather than left for a reader to discover.
        assert!(
            !install_refusal(true),
            "--cloud asked for the downloads; nothing should be installed"
        );
        assert!(install_refusal(false), "the default run must install it");
        assert_eq!(
            materialization_refused(),
            Some(true),
            "the policy was installed and the kernel does not agree it is set"
        );
    }

    #[test]
    fn only_the_unreadable_files_that_are_dataless_are_called_cloud() {
        let unreadable = vec![
            "gone.bin".to_string(),
            "evicted.mov".to_string(),
            "locked.bin".to_string(),
        ];
        // The seam: flags this test chose, for files that do not exist. A real
        // dataless file cannot be fabricated, which is the whole reason the
        // flags word is a parameter.
        let injected = |name: &str| match name {
            "evicted.mov" => Some(SF_DATALESS),
            "locked.bin" => Some(0x0000_0002), // UF_IMMUTABLE: unreadable, not cloud
            _ => None,                         // gone: no flags at all to read
        };
        assert_eq!(in_cloud(&unreadable, injected), ["evicted.mov"]);
        // A run that met no cloud file says nothing, which is the fence.
        assert!(in_cloud(&unreadable, |_| Some(0)).is_empty());
        assert!(in_cloud(&[], injected).is_empty());
    }

    #[test]
    fn a_read_refused_by_the_policy_is_the_error_the_path_already_handles() {
        // Fault injection: the exact error the kernel raises when the policy
        // stops a materialization. Nothing in the scan branches on it — the
        // point of the assertion is that this is an ordinary `io::Error`,
        // indistinguishable at the read-error path from a permission failure,
        // so the file leaves its group by the route every unreadable file
        // already takes.
        let refused = io::Error::from_raw_os_error(libc::EDEADLK);
        assert!(refused_materialization(&refused));
        assert_eq!(refused.kind(), io::ErrorKind::Deadlock);
        // ...and the errors it must not be confused with.
        assert!(!refused_materialization(&io::Error::from_raw_os_error(
            libc::EACCES
        )));
        assert!(!refused_materialization(&io::Error::from_raw_os_error(
            libc::ENOENT
        )));
        assert!(!refused_materialization(&io::Error::other("not an errno")));
    }

    #[test]
    fn the_notice_appears_only_when_there_is_something_to_notice() {
        assert_eq!(notice(0), None, "a clean tree says nothing at all");
        assert_eq!(
            notice(1).unwrap(),
            "1 file in cloud — not scanned (--cloud to include)"
        );
        assert_eq!(
            notice(2).unwrap(),
            "2 files in cloud — not scanned (--cloud to include)"
        );
    }

    #[test]
    fn the_eviction_note_names_the_file_and_says_where_it_went() {
        assert_eq!(evicted_notice(&[]), None);
        let one = evicted_notice(&["movies/holiday.MOV".to_string()]).unwrap();
        assert!(one.contains("movies/holiday.MOV"), "{one}");
        assert!(one.contains("lives in the cloud"), "{one}");
        assert!(one.contains("--cloud"), "{one}");
        assert!(!one.contains("and 0 more"), "{one}");
        let two = evicted_notice(&["a.MOV".to_string(), "b.MOV".to_string()]).unwrap();
        assert!(
            two.contains("a.MOV") && two.contains("(and 1 more)"),
            "{two}"
        );
        let many = evicted_notice(&[
            "a.MOV".to_string(),
            "b.MOV".to_string(),
            "c.MOV".to_string(),
        ])
        .unwrap();
        assert!(many.contains("(and 2 more)"), "{many}");
    }

    #[test]
    fn a_scan_counts_what_it_skipped_and_what_it_lost_to_an_eviction() {
        let cloud = Cloud::default();
        assert_eq!(cloud.skipped(), 0);
        assert!(cloud.evicted_names().is_empty());
        cloud.skip();
        cloud.skip();
        assert_eq!(cloud.skipped(), 2);
        // An eviction is a file in the cloud this scan did not read, which is
        // the same thing the walk-time skips are, so it joins the same count.
        cloud.evicted(vec!["late.MOV".to_string()]);
        assert_eq!(cloud.skipped(), 3);
        assert_eq!(cloud.evicted_names(), ["late.MOV"]);
        // Nothing evicted changes nothing.
        cloud.evicted(Vec::new());
        assert_eq!(cloud.skipped(), 3);
    }
}
