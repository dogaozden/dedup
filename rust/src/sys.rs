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

// IOKit and CoreFoundation are loaded on first use rather than linked, and
// the reason is startup. `otool -L` on a build that links them shows both
// frameworks plus `libiconv`, and dyld maps and initialises all three before
// `main` runs: measured on this machine, a C program that does nothing costs
// 1.349 ms and the same program linked against the two costs 2.136 ms. That
// 0.787 ms is paid by every scan, and on the short scenarios it is a fifth of
// process start. Nothing needs the answer until the pools are sized, and
// `VolumeProbe` already asks for it from a thread, so the load can happen
// there and overlap the walk instead of preceding it.
//
// `-Wl,-lazy_framework` would have been the free version of this and the
// linker on this machine refuses it: "lazy-load will be ignored ... because
// deployment target version is too low", at every deployment target tried.
//
// Every entry point below keeps the name and signature it had as an `extern`
// declaration, so no call site knows the difference. A framework that will not
// load, or a symbol that is not there, leaves `frameworks()` at `None` and
// each wrapper returns what the real call returns when it fails — which the
// callers already handle, because a volume that will not answer is a volume
// this has always had to cope with.
#[allow(non_snake_case)]
struct Frameworks {
    IOBSDNameMatching: unsafe extern "C" fn(IoObject, u32, *const c_char) -> *const c_void,
    IOServiceGetMatchingService: unsafe extern "C" fn(IoObject, *const c_void) -> IoObject,
    IORegistryEntrySearchCFProperty:
        unsafe extern "C" fn(IoObject, *const c_char, CfTypeRef, CfTypeRef, u32) -> CfTypeRef,
    IOObjectRelease: unsafe extern "C" fn(IoObject) -> c_int,
    IOObjectConformsTo: unsafe extern "C" fn(IoObject, *const c_char) -> u8,
    IORegistryEntryGetParentEntry:
        unsafe extern "C" fn(IoObject, *const c_char, *mut IoObject) -> c_int,
    CFStringCreateWithCString: unsafe extern "C" fn(CfTypeRef, *const c_char, u32) -> CfTypeRef,
    CFGetTypeID: unsafe extern "C" fn(CfTypeRef) -> usize,
    CFDictionaryGetTypeID: unsafe extern "C" fn() -> usize,
    CFStringGetTypeID: unsafe extern "C" fn() -> usize,
    CFDictionaryGetValue: unsafe extern "C" fn(CfTypeRef, CfTypeRef) -> CfTypeRef,
    CFStringGetCString: unsafe extern "C" fn(CfTypeRef, *mut c_char, isize, u32) -> u8,
    CFRelease: unsafe extern "C" fn(CfTypeRef),
}

static FRAMEWORKS: std::sync::OnceLock<Option<Frameworks>> = std::sync::OnceLock::new();

fn frameworks() -> Option<&'static Frameworks> {
    FRAMEWORKS
        .get_or_init(|| unsafe { load_frameworks() })
        .as_ref()
}

/// The handles are deliberately never closed: the process needs them for as
/// long as it runs, and `dlclose` on a system framework is not something to do
/// on the way out.
// The destination types are the `Frameworks` fields themselves — annotating
// each transmute would spell every signature twice.
#[allow(clippy::missing_transmute_annotations)]
unsafe fn load_frameworks() -> Option<Frameworks> {
    unsafe fn open(path: &[u8]) -> *mut c_void {
        libc::dlopen(path.as_ptr().cast(), libc::RTLD_LAZY)
    }
    let iokit = open(b"/System/Library/Frameworks/IOKit.framework/IOKit\0");
    if iokit.is_null() {
        return None;
    }
    let cf = open(b"/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation\0");
    if cf.is_null() {
        return None;
    }
    macro_rules! sym {
        ($handle:expr, $name:literal) => {{
            let found = libc::dlsym($handle, concat!($name, "\0").as_ptr().cast());
            if found.is_null() {
                return None;
            }
            std::mem::transmute(found)
        }};
    }
    Some(Frameworks {
        IOBSDNameMatching: sym!(iokit, "IOBSDNameMatching"),
        IOServiceGetMatchingService: sym!(iokit, "IOServiceGetMatchingService"),
        IORegistryEntrySearchCFProperty: sym!(iokit, "IORegistryEntrySearchCFProperty"),
        IOObjectRelease: sym!(iokit, "IOObjectRelease"),
        IOObjectConformsTo: sym!(iokit, "IOObjectConformsTo"),
        IORegistryEntryGetParentEntry: sym!(iokit, "IORegistryEntryGetParentEntry"),
        CFStringCreateWithCString: sym!(cf, "CFStringCreateWithCString"),
        CFGetTypeID: sym!(cf, "CFGetTypeID"),
        CFDictionaryGetTypeID: sym!(cf, "CFDictionaryGetTypeID"),
        CFStringGetTypeID: sym!(cf, "CFStringGetTypeID"),
        CFDictionaryGetValue: sym!(cf, "CFDictionaryGetValue"),
        CFStringGetCString: sym!(cf, "CFStringGetCString"),
        CFRelease: sym!(cf, "CFRelease"),
    })
}

// The wrappers. Each one is the call it replaced, or that call's own failure
// answer when the frameworks are not there.
#[allow(non_snake_case)]
mod shim {
    use super::*;

    pub unsafe fn IOBSDNameMatching(
        port: IoObject,
        options: u32,
        name: *const c_char,
    ) -> *const c_void {
        match frameworks() {
            Some(f) => (f.IOBSDNameMatching)(port, options, name),
            None => std::ptr::null(),
        }
    }
    pub unsafe fn IOServiceGetMatchingService(port: IoObject, matching: *const c_void) -> IoObject {
        match frameworks() {
            Some(f) => (f.IOServiceGetMatchingService)(port, matching),
            None => 0,
        }
    }
    pub unsafe fn IORegistryEntrySearchCFProperty(
        entry: IoObject,
        plane: *const c_char,
        key: CfTypeRef,
        allocator: CfTypeRef,
        options: u32,
    ) -> CfTypeRef {
        match frameworks() {
            Some(f) => (f.IORegistryEntrySearchCFProperty)(entry, plane, key, allocator, options),
            None => std::ptr::null(),
        }
    }
    pub unsafe fn IOObjectRelease(object: IoObject) -> c_int {
        match frameworks() {
            Some(f) => (f.IOObjectRelease)(object),
            None => 0,
        }
    }
    pub unsafe fn IOObjectConformsTo(object: IoObject, class_name: *const c_char) -> u8 {
        match frameworks() {
            Some(f) => (f.IOObjectConformsTo)(object, class_name),
            None => 0,
        }
    }
    pub unsafe fn IORegistryEntryGetParentEntry(
        entry: IoObject,
        plane: *const c_char,
        parent: *mut IoObject,
    ) -> c_int {
        match frameworks() {
            Some(f) => (f.IORegistryEntryGetParentEntry)(entry, plane, parent),
            None => -1,
        }
    }
    pub unsafe fn CFStringCreateWithCString(
        allocator: CfTypeRef,
        c_string: *const c_char,
        encoding: u32,
    ) -> CfTypeRef {
        match frameworks() {
            Some(f) => (f.CFStringCreateWithCString)(allocator, c_string, encoding),
            None => std::ptr::null(),
        }
    }
    // The three type-id calls answer distinct nonsense when there is no
    // CoreFoundation, so that a comparison between them is false rather than
    // accidentally true.
    pub unsafe fn CFGetTypeID(value: CfTypeRef) -> usize {
        match frameworks() {
            Some(f) => (f.CFGetTypeID)(value),
            None => 0,
        }
    }
    pub unsafe fn CFDictionaryGetTypeID() -> usize {
        match frameworks() {
            Some(f) => (f.CFDictionaryGetTypeID)(),
            None => 1,
        }
    }
    pub unsafe fn CFStringGetTypeID() -> usize {
        match frameworks() {
            Some(f) => (f.CFStringGetTypeID)(),
            None => 2,
        }
    }
    pub unsafe fn CFDictionaryGetValue(dictionary: CfTypeRef, key: CfTypeRef) -> CfTypeRef {
        match frameworks() {
            Some(f) => (f.CFDictionaryGetValue)(dictionary, key),
            None => std::ptr::null(),
        }
    }
    pub unsafe fn CFStringGetCString(
        value: CfTypeRef,
        buffer: *mut c_char,
        size: isize,
        encoding: u32,
    ) -> u8 {
        match frameworks() {
            Some(f) => (f.CFStringGetCString)(value, buffer, size, encoding),
            None => 0,
        }
    }
    pub unsafe fn CFRelease(value: CfTypeRef) {
        if let Some(f) = frameworks() {
            (f.CFRelease)(value);
        }
    }
}
use shim::*;

/// How many cores of the fastest kind this machine has, on a machine whose
/// cores are not all alike.
///
/// Apple silicon numbers its performance levels fastest first, so
/// `perflevel0` is the performance cores and `perflevel1` the efficiency
/// ones. A machine with a single performance level answers with all of its
/// cores, which is the same answer as not distinguishing, and one that does
/// not carry the key at all says `None`.
pub fn performance_cores() -> Option<usize> {
    let name = CString::new("hw.perflevel0.logicalcpu").expect("no interior NUL");
    let mut cores: u32 = 0;
    let mut size = std::mem::size_of::<u32>();
    // SAFETY: `name` is NUL-terminated and outlives the call; `cores` is
    // writable storage of exactly `size` bytes, which is what a sysctl of
    // this integer type writes. A nonzero return means it wrote nothing.
    let answered = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            std::ptr::addr_of_mut!(cores).cast::<c_void>(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if answered != 0 || size != std::mem::size_of::<u32>() || cores == 0 {
        return None;
    }
    Some(cores as usize)
}

/// Whether the volume `path` sits on is backed by a disk image rather than by
/// a physical device — a RAM disk, a `.dmg`, a sparsebundle, a VM volume.
///
/// This is a different question from `solid_state`, and it is asked because
/// the thing the tool actually wants to know is whether the volume charges
/// for a seek. `solid_state` answers `None` for every disk image, since the
/// registry carries no `Medium Type` for one — the same answer it gives for a
/// USB enclosure that simply does not report, which is a device that may very
/// well spin. Treating those two alike costs a disk image the parallel
/// reader, and treating them alike in the other direction would cost a real
/// spinning disk a seek storm. The registry separates them: a disk image is
/// provided by `IODiskImageBlockStorageDeviceOutKernel`, and nothing with
/// heads is.
///
/// `false` for anything the registry will not describe, which leaves the
/// caller exactly where it was.
/// What the device under a volume says about itself, in two ioctls.
///
/// `diskutil info` prints these as `SolidState` and `VirtualOrPhysical`, and
/// gets them the same way this does: one open of the raw node and one ioctl
/// each. That costs about forty microseconds. The registry below answers the
/// same two questions and costs two to four milliseconds, almost all of it
/// loading IOKit and CoreFoundation — a fixed price paid once per scan, which
/// the scenarios that finish in ten milliseconds feel as a fifth of their run.
///
/// `None` means the device would not answer. The raw node for a system volume
/// is `root:operator` and an ordinary user cannot open it, so this settles the
/// question for a volume the user attached and leaves the rest to the
/// registry, which is what ran here before and still runs when this declines.
pub struct MediaFlags {
    /// The disk is flash. `false` is "the device said no", not "did not say" —
    /// which is why this cannot stand in for the registry's tri-state: a RAM
    /// disk answers no here and "no such key" there.
    pub solid: bool,
    /// The block device is synthesized rather than physical — a disk image.
    /// This is the same population `disk_image` identifies by conformance.
    pub virtual_media: bool,
}

/// `_IOR('d', 79, uint32_t)` and `_IOR('d', 72, uint32_t)` from `<sys/disk.h>`,
/// spelled out because there is no macro on this side to expand them.
const DKIOCISSOLIDSTATE: libc::c_ulong = 0x4004_644f;
const DKIOCISVIRTUAL: libc::c_ulong = 0x4004_6448;

pub fn media_flags(path: &Path) -> Option<MediaFlags> {
    let device = backing_device(path)?;
    // The raw node, not the block node: the block one is busy while the
    // volume is mounted and would refuse the open.
    let mut node = Vec::with_capacity(6 + device.as_bytes().len());
    node.extend_from_slice(b"/dev/r");
    node.extend_from_slice(device.as_bytes());
    let node = CString::new(node).ok()?;

    // SAFETY: `node` is NUL-terminated and outlives the call.
    let fd = unsafe { libc::open(node.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    let mut solid: u32 = 0;
    let mut synthesized: u32 = 0;
    // SAFETY: both requests are `_IOR`s of one `uint32_t` and each is handed a
    // writable `u32` of exactly that size. `fd` came from a successful open.
    let (asked_solid, asked_virtual) = unsafe {
        (
            libc::ioctl(fd, DKIOCISSOLIDSTATE, &mut solid) == 0,
            libc::ioctl(fd, DKIOCISVIRTUAL, &mut synthesized) == 0,
        )
    };
    // SAFETY: `fd` came from a successful `open` and is closed once.
    unsafe { libc::close(fd) };

    (asked_solid || asked_virtual).then_some(MediaFlags {
        solid: asked_solid && solid != 0,
        virtual_media: asked_virtual && synthesized != 0,
    })
}

pub fn disk_image(path: &Path) -> bool {
    let Some(device) = backing_device(path) else {
        return false;
    };
    let plane = CString::new("IOService").expect("no interior NUL");
    let class = CString::new("IODiskImageBlockStorageDeviceOutKernel").expect("no interior NUL");

    // SAFETY: `IOServiceGetMatchingService` consumes the matching dictionary,
    // so it is not released here. Every entry this loop holds is released
    // exactly once: `entry` is replaced only after the old value is released,
    // and whatever it holds on the way out is released after the loop. A zero
    // `io_object_t` is not an object and is not released.
    unsafe {
        let matching = IOBSDNameMatching(K_IO_MAIN_PORT_DEFAULT, 0, device.as_ptr());
        if matching.is_null() {
            return false;
        }
        let mut entry = IOServiceGetMatchingService(K_IO_MAIN_PORT_DEFAULT, matching);
        if entry == 0 {
            return false;
        }
        // The stack over a disk image is three or four deep; the bound is
        // there so a registry that somehow loops cannot hang a scan.
        let mut virtual_media = false;
        for _ in 0..16 {
            if IOObjectConformsTo(entry, class.as_ptr()) != 0 {
                virtual_media = true;
                break;
            }
            let mut parent: IoObject = 0;
            if IORegistryEntryGetParentEntry(entry, plane.as_ptr(), &mut parent) != 0 {
                break;
            }
            IOObjectRelease(entry);
            entry = parent;
        }
        IOObjectRelease(entry);
        virtual_media
    }
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

    /// The ioctls have to say what the registry says, or the fast path is a
    /// different tool rather than a faster one.
    ///
    /// On a machine whose only user-openable volume is the bench RAM disk this
    /// asserts about one volume and skips the boot disk, which is honest: the
    /// boot disk is precisely the case that still goes to the registry, and
    /// `the_registry_agrees_with_diskutil` is what covers it.
    #[test]
    fn the_device_agrees_with_diskutil() {
        for mount in ["/", "/Volumes/dedupbench"] {
            let path = Path::new(mount);
            if !path.exists() {
                continue;
            }
            let Some(media) = media_flags(path) else {
                println!("{mount}: raw node would not open — the registry answers");
                continue;
            };
            let virtual_media = diskutil_string(mount, "VirtualOrPhysical");
            println!(
                "{mount}: solid={} virtual={} diskutil={:?}/{:?}",
                media.solid,
                media.virtual_media,
                diskutil_says(mount),
                virtual_media,
            );
            assert_eq!(
                media.virtual_media,
                virtual_media.as_deref() == Some("Virtual"),
                "{mount}: disagreed with diskutil about being a disk image",
            );
            // Only where diskutil has an opinion. "Info not available" is the
            // registry declining, and the device answering `no` to a question
            // the registry would not answer at all is not a disagreement.
            if let Some(theirs) = diskutil_says(mount) {
                assert_eq!(media.solid, theirs, "{mount}: disagreed about flash");
            }
            // The one direction that would be a real conflict.
            if media.solid {
                assert_ne!(
                    solid_state(path),
                    Some(false),
                    "{mount}: the device says flash and the registry says it spins",
                );
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

// ---------------------------------------------------------------------------
// Bulk directory reads
//
// REWRITE.md §4 wants the walk to stop paying for one `lstat` per file. The
// kernel will hand over a whole directory's names *and* their attributes in
// one call, which is what `getattrlistbulk(2)` is for, and on HFS+ and APFS
// that is one pass over the catalog instead of a lookup per entry.
//
// Unlike the rest of this module this path *is* load-bearing: it produces the
// file list. It keeps the module's contract anyway — `BulkDir::read` answers
// `false` for anything the platform will not describe completely, and the
// walk falls back to `read_dir` for that directory. It never answers `true`
// having seen only part of a directory, so a caller can fall back without
// risking a double count.
// ---------------------------------------------------------------------------

const ATTR_BIT_MAP_COUNT: u16 = 5;
const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
const ATTR_CMN_NAME: u32 = 0x0000_0001;
const ATTR_CMN_DEVID: u32 = 0x0000_0002;
const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
const ATTR_CMN_MODTIME: u32 = 0x0000_0400;
const ATTR_CMN_FILEID: u32 = 0x0200_0000;
const ATTR_FILE_LINKCOUNT: u32 = 0x0000_0001;
const ATTR_FILE_DATALENGTH: u32 = 0x0000_0200;

/// `enum vtype` — the three this walk distinguishes. Everything else is
/// neither a file nor a directory and is skipped exactly as `read_dir`'s
/// `is_file`/`is_dir` pair skips it.
pub const VREG: u32 = 1;
pub const VDIR: u32 = 2;
pub const VLNK: u32 = 5;

/// What every entry must carry before the walk will trust the batch.
const WANT_COMMON: u32 = ATTR_CMN_RETURNED_ATTRS
    | ATTR_CMN_NAME
    | ATTR_CMN_DEVID
    | ATTR_CMN_OBJTYPE
    | ATTR_CMN_MODTIME
    | ATTR_CMN_FILEID;
const WANT_FILE: u32 = ATTR_FILE_LINKCOUNT | ATTR_FILE_DATALENGTH;

/// One directory's worth of buffer, sized so an ordinary directory comes back
/// in a single call and a large one in a handful.
const BULK_BUFFER: usize = 128 * 1024;

#[repr(C)]
struct AttrList {
    bitmapcount: u16,
    reserved: u16,
    commonattr: u32,
    volattr: u32,
    dirattr: u32,
    fileattr: u32,
    forkattr: u32,
}

extern "C" {
    fn getattrlistbulk(
        dirfd: c_int,
        attrlist: *mut c_void,
        buffer: *mut c_void,
        buffersize: usize,
        options: u64,
    ) -> c_int;
}

/// One directory entry, as the kernel described it. `name` is an offset into
/// the arena rather than a `String` so that a directory of 50,000 entries
/// costs one allocation rather than 50,000.
pub struct BulkEntry {
    name_at: u32,
    name_len: u32,
    pub objtype: u32,
    pub dev: u64,
    pub ino: u64,
    pub nlink: u64,
    pub size: u64,
    pub mtime_ns: i64,
}

/// Scratch reused across every directory of a walk: the kernel's buffer, the
/// name arena, and the parsed entries.
#[derive(Default)]
pub struct BulkDir {
    buffer: Vec<u8>,
    names: Vec<u8>,
    entries: Vec<BulkEntry>,
}

/// Read `offset` bytes into `buffer` as a `T`. The kernel packs these fields
/// end to end with no padding, so almost none of them are aligned and every
/// read has to say so.
///
/// SAFETY: the caller has checked that `offset + size_of::<T>()` is within
/// `buffer`, and every `T` used here is a plain integer with no invalid bit
/// patterns.
unsafe fn unpack<T: Copy>(buffer: &[u8], offset: usize) -> T {
    std::ptr::read_unaligned(buffer.as_ptr().add(offset).cast::<T>())
}

impl BulkDir {
    /// Fill this scratch with `directory`'s entries.
    ///
    /// `false` means the platform did not describe the directory completely
    /// and the caller must fall back; nothing read here is then usable, and
    /// no caller has been told about any of it yet.
    pub fn read(&mut self, directory: &Path) -> bool {
        self.names.clear();
        self.entries.clear();
        if self.buffer.len() != BULK_BUFFER {
            self.buffer.resize(BULK_BUFFER, 0);
        }

        let Ok(c_path) = CString::new(directory.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: `c_path` is NUL-terminated and outlives the call.
        let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        if fd < 0 {
            return false;
        }

        let mut request = AttrList {
            bitmapcount: ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: WANT_COMMON,
            volattr: 0,
            dirattr: 0,
            fileattr: WANT_FILE,
            forkattr: 0,
        };

        let complete = loop {
            // SAFETY: `request` is a well-formed attrlist that outlives the
            // call, and the kernel writes at most `BULK_BUFFER` bytes into a
            // buffer of exactly that size.
            let count = unsafe {
                getattrlistbulk(
                    fd,
                    std::ptr::addr_of_mut!(request).cast::<c_void>(),
                    self.buffer.as_mut_ptr().cast::<c_void>(),
                    BULK_BUFFER,
                    0,
                )
            };
            if count < 0 {
                break false;
            }
            if count == 0 {
                break true;
            }
            if !self.parse(count as usize) {
                break false;
            }
        };

        // SAFETY: `fd` came from a successful `open` and is closed once.
        unsafe { libc::close(fd) };
        complete
    }

    /// Split one batch of `count` entries out of the buffer.
    fn parse(&mut self, count: usize) -> bool {
        let mut base = 0_usize;
        for _ in 0..count {
            if base + 4 > self.buffer.len() {
                return false;
            }
            // SAFETY: bounds checked immediately above and below; every read
            // below stays inside `base .. base + length`.
            let length = unsafe { unpack::<u32>(&self.buffer, base) } as usize;
            if length < 24 || base + length > self.buffer.len() {
                return false;
            }
            if !self.parse_entry(base, length) {
                return false;
            }
            base += length;
        }
        true
    }

    fn parse_entry(&mut self, base: usize, length: usize) -> bool {
        let end = base + length;
        let mut at = base + 4;

        // SAFETY: `at + 20 <= end <= buffer.len()`, checked by the caller's
        // `length < 24` guard.
        let returned_common = unsafe { unpack::<u32>(&self.buffer, at) };
        let returned_file = unsafe { unpack::<u32>(&self.buffer, at + 12) };
        at += 20;

        // Anything the kernel declined to describe sends the whole directory
        // down the portable path rather than being guessed at.
        if returned_common & WANT_COMMON != WANT_COMMON {
            return false;
        }

        macro_rules! take {
            ($ty:ty) => {{
                let size = std::mem::size_of::<$ty>();
                if at + size > end {
                    return false;
                }
                // SAFETY: bounds checked on the line above.
                let value = unsafe { unpack::<$ty>(&self.buffer, at) };
                at += size;
                value
            }};
        }

        // The order is the order of the bits in `struct attrlist`: common
        // attributes low bit first, then the file group.
        //
        // `attr_dataoffset` counts from the reference itself, so where that
        // reference sits has to be remembered before the cursor moves on.
        let name_reference = at;
        let name_offset = take!(i32);
        let name_length = take!(u32) as usize;
        let dev = take!(i32) as u64;
        let objtype = take!(u32);
        let mtime_sec = take!(i64);
        let mtime_nsec = take!(i64);
        let ino = take!(u64);

        let name_start = name_reference as i64 + name_offset as i64;
        if name_start < base as i64 || name_length == 0 {
            return false;
        }
        let name_start = name_start as usize;
        // The length the kernel reports includes the trailing NUL.
        let name_end = name_start + name_length - 1;
        if name_end > end {
            return false;
        }

        let (nlink, size) = if objtype == VREG {
            if returned_file & WANT_FILE != WANT_FILE {
                return false;
            }
            let nlink = take!(u32) as u64;
            let size = take!(i64) as u64;
            (nlink, size)
        } else {
            (0, 0)
        };

        // Every field the kernel said it returned has now been accounted for,
        // and none of them ran past the entry it belongs to.
        debug_assert!(at <= end);

        let name_at = self.names.len() as u32;
        self.names
            .extend_from_slice(&self.buffer[name_start..name_end]);
        self.entries.push(BulkEntry {
            name_at,
            name_len: (name_end - name_start) as u32,
            objtype,
            dev,
            ino,
            nlink,
            size,
            mtime_ns: mtime_sec * 1_000_000_000 + mtime_nsec,
        });
        true
    }

    /// The entries of the directory last read, each with its raw name.
    pub fn entries(&self) -> impl Iterator<Item = (&[u8], &BulkEntry)> {
        self.entries.iter().map(|entry| {
            let at = entry.name_at as usize;
            (&self.names[at..at + entry.name_len as usize], entry)
        })
    }
}

#[cfg(test)]
mod virtual_media {
    use super::*;

    /// The boot volume is a real device, and nothing about it is an image.
    #[test]
    fn a_physical_volume_is_not_a_disk_image() {
        assert!(!disk_image(Path::new("/")));
    }

    /// The bench volume is a RAM disk, which is a disk image with memory
    /// behind it. Skipped where it is not mounted, because this is the only
    /// virtual volume this machine is guaranteed to have when it is.
    #[test]
    fn a_ram_disk_is_a_disk_image() {
        let bench = Path::new("/Volumes/dedupbench");
        if !bench.exists() {
            return;
        }
        assert!(disk_image(bench), "the bench RAM disk reads as physical");
    }

    /// Asking about a path that is not there says no, rather than guessing.
    #[test]
    fn a_path_that_is_not_there_is_not_a_disk_image() {
        assert!(!disk_image(Path::new("/no/such/place")));
    }
}
