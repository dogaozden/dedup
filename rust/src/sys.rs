//! The platform questions this tool cannot ask in portable Rust.
//!
//! REWRITE.md §7 confines `unsafe` to one FFI module and asks that anything
//! beyond it be justified in LOG.md first; this is that module and that
//! justification is written there. Everything here answers a question the
//! tool used to answer by running `diskutil` and waiting for it — a
//! subprocess that costs more than most scans do.
//!
//! Nothing here is load-bearing for correctness. Every function returns
//! `None` when the platform will not say, which is the same answer the
//! subprocess gave for a volume it did not recognise, and every caller
//! already treats "don't know" as the conservative case.

use std::ffi::{c_char, c_int, c_void, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Duration;

/// The BSD device name backing `path` — "disk4s1" and the like — or `None`
/// for anything `statfs` will not describe.
fn backing_device(path: &Path) -> Option<CString> {
    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut buffer = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `c_path` is a NUL-terminated C string that outlives the call,
    // and `buffer` is writable, correctly aligned storage of exactly the size
    // the kernel fills. A nonzero return means it wrote nothing, and the
    // uninitialised buffer is dropped without being read.
    if unsafe { libc::statfs(c_path.as_ptr(), buffer.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: statfs returned 0, so every field is initialised.
    let stat = unsafe { buffer.assume_init() };
    let raw: Vec<u8> = stat
        .f_mntfromname
        .iter()
        .take_while(|&&byte| byte != 0)
        .map(|&byte| byte as u8)
        .collect();
    let name = raw.strip_prefix(b"/dev/".as_slice())?;
    CString::new(name).ok()
}

type IoObject = u32;
type CfTypeRef = *const c_void;

const K_IO_MAIN_PORT_DEFAULT: IoObject = 0;
/// Walk towards the root of the IOService plane as well as down it: the flag
/// lives on the physical device, and what `statfs` names is a partition.
const K_IO_REGISTRY_ITERATE_RECURSIVELY: u32 = 1;
const K_IO_REGISTRY_ITERATE_PARENTS: u32 = 2;
const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOBSDNameMatching(
        main_port: IoObject,
        options: u32,
        bsd_name: *const c_char,
    ) -> *const c_void;
    fn IOServiceGetMatchingService(main_port: IoObject, matching: *const c_void) -> IoObject;
    fn IORegistryEntrySearchCFProperty(
        entry: IoObject,
        plane: *const c_char,
        key: CfTypeRef,
        allocator: CfTypeRef,
        options: u32,
    ) -> CfTypeRef;
    fn IOObjectRelease(object: IoObject) -> c_int;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(
        allocator: CfTypeRef,
        c_string: *const c_char,
        encoding: u32,
    ) -> CfTypeRef;
    fn CFGetTypeID(value: CfTypeRef) -> usize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFDictionaryGetValue(dictionary: CfTypeRef, key: CfTypeRef) -> CfTypeRef;
    fn CFStringGetCString(value: CfTypeRef, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFRelease(value: CfTypeRef);
}

/// Whether the volume `path` sits on is flash, as the IO registry has it.
///
/// This is the same fact `diskutil info` prints as `SolidState`, read from the
/// same place `diskutil` reads it, without the process in between. The
/// registry keeps it as `Medium Type` inside the block storage device's
/// `Device Characteristics`, one entry per physical disk — so the search
/// climbs towards the root of the IOService plane, because what `statfs`
/// names is a partition, or on APFS a volume inside a container inside a
/// partition. `None` means no such entry carries the key, which is what a
/// disk image, a RAM disk and most USB enclosures look like, and is exactly
/// the answer the subprocess gave for them.
pub fn solid_state(path: &Path) -> Option<bool> {
    let device = backing_device(path)?;
    let plane = CString::new("IOService").expect("no interior NUL");
    let property = CString::new("Device Characteristics").expect("no interior NUL");
    let medium = CString::new("Medium Type").expect("no interior NUL");

    // SAFETY: every pointer below is either NUL-terminated storage that
    // outlives its call or a CF object whose ownership follows the two CF
    // rules: `IOServiceGetMatchingService` consumes the matching dictionary,
    // so it is not released here, and everything this function is *given* back
    // (the CFString it created, the property it copied) is released on every
    // path out. A zero `io_object_t` means no match and is not released.
    unsafe {
        let matching = IOBSDNameMatching(K_IO_MAIN_PORT_DEFAULT, 0, device.as_ptr());
        if matching.is_null() {
            return None;
        }
        let service = IOServiceGetMatchingService(K_IO_MAIN_PORT_DEFAULT, matching);
        if service == 0 {
            return None;
        }
        let key = CFStringCreateWithCString(
            std::ptr::null(),
            property.as_ptr(),
            K_CF_STRING_ENCODING_UTF8,
        );
        if key.is_null() {
            IOObjectRelease(service);
            return None;
        }
        let value = IORegistryEntrySearchCFProperty(
            service,
            plane.as_ptr(),
            key,
            std::ptr::null(),
            K_IO_REGISTRY_ITERATE_RECURSIVELY | K_IO_REGISTRY_ITERATE_PARENTS,
        );
        CFRelease(key);
        IOObjectRelease(service);
        if value.is_null() {
            return None;
        }
        // A property of the wrong type is a registry we do not understand,
        // not a disk we know something about.
        let answer = (CFGetTypeID(value) == CFDictionaryGetTypeID())
            .then(|| medium_type(value, &medium))
            .flatten();
        CFRelease(value);
        answer
    }
}

/// `Medium Type` out of a `Device Characteristics` dictionary. Borrowed, not
/// copied — `CFDictionaryGetValue` hands back a reference the dictionary still
/// owns, so nothing here is released.
///
/// # Safety
/// `characteristics` must be a live CFDictionary and `key` a NUL-terminated
/// C string; both are guaranteed by the single caller.
unsafe fn medium_type(characteristics: CfTypeRef, key: &CString) -> Option<bool> {
    let name = CFStringCreateWithCString(std::ptr::null(), key.as_ptr(), K_CF_STRING_ENCODING_UTF8);
    if name.is_null() {
        return None;
    }
    let value = CFDictionaryGetValue(characteristics, name);
    CFRelease(name);
    if value.is_null() || CFGetTypeID(value) != CFStringGetTypeID() {
        return None;
    }
    let mut buffer = [0 as c_char; 64];
    if CFStringGetCString(
        value,
        buffer.as_mut_ptr(),
        buffer.len() as isize,
        K_CF_STRING_ENCODING_UTF8,
    ) == 0
    {
        return None;
    }
    let text: Vec<u8> = buffer
        .iter()
        .take_while(|&&byte| byte != 0)
        .map(|&byte| byte as u8)
        .collect();
    match text.as_slice() {
        b"Solid State" => Some(true),
        b"Rotational" => Some(false),
        // Anything else is a medium this tool has no opinion about, which is
        // the same as not knowing.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_boot_volume_names_a_backing_device() {
        // Every mounted volume has one; the point is that statfs is being
        // read correctly rather than returning a truncated or shifted field.
        let device = backing_device(Path::new("/")).expect("the boot volume has a device");
        let name = device.to_str().expect("ASCII");
        assert!(name.starts_with("disk"), "{name} is not a BSD disk name");
    }

    #[test]
    fn asking_about_a_path_that_is_not_there_says_nothing() {
        assert_eq!(solid_state(Path::new("/no/such/place")), None);
    }
}

#[cfg(test)]
mod agreement {
    use super::*;
    use std::process::Command;

    /// What `diskutil info` says, which is what this module replaced.
    fn diskutil_says(mount: &str) -> Option<bool> {
        let out = Command::new("diskutil")
            .args(["info", "-plist", mount])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let rest = text.split_once("<key>SolidState</key>")?.1.trim_start();
        Some(rest.starts_with("<true"))
    }

    /// Whatever `diskutil info` prints for a key, as text.
    fn diskutil_string(mount: &str, key: &str) -> Option<String> {
        let out = Command::new("diskutil")
            .args(["info", "-plist", mount])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let rest = text
            .split_once(&format!("<key>{key}</key>"))?
            .1
            .trim_start();
        let value = rest.strip_prefix("<string>")?.split('<').next()?;
        Some(value.to_string())
    }

    /// The UUID names a file in the cache directory, so getting the same
    /// sixteen bytes is not enough — it has to be spelled the way the
    /// subprocess spelled it, or every volume silently starts again from cold.
    #[test]
    fn the_volume_uuid_is_spelled_the_way_diskutil_spells_it() {
        for mount in ["/", "/Volumes/dedupbench"] {
            let path = Path::new(mount);
            if !path.exists() {
                continue;
            }
            let (ours, theirs) = (volume_uuid(path), diskutil_string(mount, "VolumeUUID"));
            println!("{mount}: getattrlist={ours:?} diskutil={theirs:?}");
            if theirs.is_some() {
                assert_eq!(ours, theirs, "{mount}: disagreed with diskutil");
            }
        }
    }

    /// The replacement has to give the answer it replaced, on whatever volumes
    /// this machine happens to have mounted. A faster way to a different answer
    /// would be a different tool, not a faster one.
    #[test]
    fn the_registry_agrees_with_diskutil() {
        for mount in ["/", "/Volumes/dedupbench"] {
            let path = Path::new(mount);
            if !path.exists() {
                continue;
            }
            let (ours, theirs) = (solid_state(path), diskutil_says(mount));
            println!("{mount}: registry={ours:?} diskutil={theirs:?}");
            assert_eq!(ours, theirs, "{mount}: disagreed with diskutil");
        }
    }
}

/// The UUID of the volume `path` sits on — one syscall, where this used to be
/// a second `diskutil`.
///
/// The spelling matters as much as the value: it names a file in the cache
/// directory, so a version that spelled it differently would silently start
/// every volume again from cold. `getattrlist` hands back the same sixteen
/// bytes `diskutil` prints, and this prints them the same way — uppercase,
/// hyphenated 8-4-4-4-12.
pub fn volume_uuid(path: &Path) -> Option<String> {
    const ATTR_BIT_MAP_COUNT: u16 = 5;
    const ATTR_VOL_INFO: u32 = 0x8000_0000;
    const ATTR_VOL_UUID: u32 = 0x0004_0000;

    /// What the kernel writes back: its own length, then the UUID. `length`
    /// is how the "this volume has no UUID" case is told apart from a filled
    /// buffer — an unsupported attribute is simply not written.
    #[repr(C)]
    struct Answer {
        length: u32,
        uuid: [u8; 16],
    }

    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `attrlist` is a plain C struct of integers, for which all-zero
    // is the documented "ask for nothing" state; the two fields set below are
    // the request.
    let mut request: libc::attrlist = unsafe { std::mem::zeroed() };
    request.bitmapcount = ATTR_BIT_MAP_COUNT;
    request.volattr = ATTR_VOL_INFO | ATTR_VOL_UUID;

    let mut answer = std::mem::MaybeUninit::<Answer>::uninit();
    // SAFETY: `c_path` is NUL-terminated and outlives the call; `request` is a
    // fully initialised attrlist; `answer` is writable storage of exactly the
    // size passed. A nonzero return means nothing was written and the
    // uninitialised buffer is dropped unread.
    if unsafe {
        libc::getattrlist(
            c_path.as_ptr(),
            std::ptr::addr_of_mut!(request).cast::<c_void>(),
            answer.as_mut_ptr().cast::<c_void>(),
            std::mem::size_of::<Answer>(),
            0,
        )
    } != 0
    {
        return None;
    }
    // SAFETY: getattrlist returned 0, so it wrote at least `length`.
    let answer = unsafe { answer.assume_init() };
    if answer.length as usize != std::mem::size_of::<Answer>() {
        return None; // the volume does not carry a UUID
    }
    let hex: String = answer
        .uuid
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect();
    Some(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

/// What the terminal has to say to a program about to block on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// There are bytes waiting. Reading will return something.
    Ready,
    /// The far end is gone, and every read from here on is end-of-file.
    HungUp,
    /// Nothing arrived before the timeout.
    Waited,
}

/// Wait for input on `fd`, and say whether the far end has hung up.
///
/// The browser needs this because `crossterm::event::read` cannot answer it. A
/// descriptor whose terminal has gone away is *readable* forever and *empty*
/// forever, and crossterm's Unix source answers that combination by looping on
/// `read` inside `try_read`: a zero-length read advances no parser, produces no
/// event, and is not an error, so the loop goes round again. It never returns
/// and it never fails, which is why no amount of error handling at the call
/// site catches it — an orphaned browser was measured burning a whole core this
/// way. `poll` reports the hangup that `read` will not, so the caller can leave
/// before handing control to a loop it cannot get back from.
fn wait_on(fd: c_int, timeout: Duration) -> Input {
    let mut watch = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = timeout.as_millis().min(c_int::MAX as u128) as c_int;
    // SAFETY: `watch` is one fully initialised `pollfd` and the count says one,
    // so the kernel writes only its `revents` field, inside storage this frame
    // owns. An invalid `fd` is reported in `revents`, not undefined behaviour.
    let ready = unsafe { libc::poll(std::ptr::addr_of_mut!(watch), 1, millis) };

    if ready < 0 {
        // A signal arriving mid-wait is not news — SIGWINCH does it on every
        // resize. Anything else means the descriptor cannot be waited on at
        // all, and leaving is the safe reading of that.
        let interrupted = std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted;
        return if interrupted {
            Input::Waited
        } else {
            Input::HungUp
        };
    }
    // Hangup is checked before readability because at end-of-file macOS reports
    // both at once, and it is the hangup that decides.
    if watch.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
        return Input::HungUp;
    }
    if watch.revents & libc::POLLIN != 0 {
        return Input::Ready;
    }
    Input::Waited
}

/// The same question asked of the terminal this process was given.
///
/// Standard input, specifically, because that is the descriptor crossterm
/// reads: its `tty_fd` prefers standard input whenever that is a terminal, and
/// the browser only opens when it is one.
pub fn wait_for_terminal_input(timeout: Duration) -> Input {
    wait_on(libc::STDIN_FILENO, timeout)
}

#[cfg(test)]
mod terminal {
    use super::*;

    /// A pty pair, so the readiness under test is a real terminal
    /// rather than a pipe standing in for one.
    fn pty() -> (c_int, c_int) {
        let (mut main, mut follower) = (0 as c_int, 0 as c_int);
        // SAFETY: both out-parameters are live `c_int` storage this frame owns,
        // and the four optional arguments are null, which `openpty` documents as
        // "use the defaults".
        let rc = unsafe {
            libc::openpty(
                std::ptr::addr_of_mut!(main),
                std::ptr::addr_of_mut!(follower),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, 0, "openpty failed: {}", std::io::Error::last_os_error());
        // `openpty` hands back plain descriptors, which every `fork` in this
        // binary then copies — and the suite forks `diskutil` and `id`. A child
        // holding the master means closing ours hangs nothing up, which is
        // exactly the "terminal is still alive" answer this test exists to
        // disprove. Close-on-exec is what keeps the pty ours.
        for fd in [main, follower] {
            // SAFETY: `fd` was just returned by `openpty`; `F_SETFD` takes an
            // int and touches nothing else.
            let flagged = unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
            assert_ne!(flagged, -1, "{}", std::io::Error::last_os_error());
        }
        (main, follower)
    }

    fn close(fd: c_int) {
        // SAFETY: `fd` came from `openpty` and is closed exactly once.
        unsafe { libc::close(fd) };
    }

    /// The whole point: a far end that has gone away must be reported as gone,
    /// not as an endless supply of nothing.
    ///
    /// The hangup is made with a socket rather than the pty above, because a
    /// pty's hangup can be postponed by anyone else holding the other end — and
    /// in this binary somebody does: every `Command` in the suite forks, and a
    /// child that inherits the master keeps the terminal alive until it exits.
    /// That made this test fail about a third of the time, always by exactly one
    /// `diskutil` lifetime. `shutdown` acts on the socket instead of on the
    /// descriptor count, so no stray child can delay it, and macOS reports it
    /// with the same `POLLIN | POLLHUP` a dead terminal reports. The terminal
    /// itself is covered end to end by `evidence/scripts/tui-terminal-death.py`.
    #[test]
    fn a_hangup_is_reported_as_a_hangup() {
        let mut ends = [0 as c_int; 2];
        // SAFETY: `ends` is two `c_int`s this frame owns, which is exactly what
        // socketpair fills.
        let rc = unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_STREAM,
                0,
                std::ptr::addr_of_mut!(ends).cast::<c_int>(),
            )
        };
        assert_eq!(rc, 0, "socketpair: {}", std::io::Error::last_os_error());
        let (near, far) = (ends[0], ends[1]);

        assert_eq!(
            wait_on(far, Duration::from_millis(50)),
            Input::Waited,
            "a live peer with nothing to say is not a hangup"
        );
        // SAFETY: `near` is an open socket from the pair above.
        unsafe { libc::shutdown(near, libc::SHUT_RDWR) };

        assert_eq!(wait_on(far, Duration::from_millis(50)), Input::HungUp);
        // And it stays that way, which is what made the old loop spin: the
        // answer never changes and never runs out, so a reader that only knows
        // how to wait for the next byte waits forever at full speed.
        assert_eq!(wait_on(far, Duration::from_millis(50)), Input::HungUp);
        close(near);
        close(far);
    }

    #[test]
    fn a_live_terminal_with_something_typed_at_it_is_ready() {
        let (main, follower) = pty();
        // With the newline, because a pty in its default line mode hands
        // nothing to the follower until the line is finished. The browser puts
        // the terminal in raw mode and so does not wait for one; this test is
        // not the browser, and would otherwise be testing the line discipline.
        let typed = b"q\n";
        // SAFETY: writing two bytes from a two-byte buffer this frame owns.
        let written = unsafe { libc::write(main, typed.as_ptr().cast::<c_void>(), 2) };
        assert_eq!(written, 2);
        assert_eq!(wait_on(follower, Duration::from_millis(500)), Input::Ready);
        close(follower);
        close(main);
    }

    /// The idle case has to stay idle: a live terminal with nothing typed at it
    /// is neither ready nor hung up, or the browser would quit on its own.
    #[test]
    fn a_live_terminal_sitting_idle_just_waits() {
        let (main, follower) = pty();
        assert_eq!(wait_on(follower, Duration::from_millis(50)), Input::Waited);
        close(follower);
        close(main);
    }
}
