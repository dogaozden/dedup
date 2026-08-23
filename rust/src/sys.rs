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
