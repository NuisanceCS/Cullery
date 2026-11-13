//! The words, and a few facts about the computer, that differ between Windows and macOS.

/// How much memory the computer has, in bytes.
#[cfg(windows)]
pub fn total_memory() -> Option<u64> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    // SAFETY: fills in a struct whose size is given.
    unsafe { GlobalMemoryStatusEx(&mut status) }.ok()?;
    Some(status.ullTotalPhys)
}

/// How much memory the computer has, in bytes.
#[cfg(target_os = "macos")]
pub fn total_memory() -> Option<u64> {
    extern "C" {
        fn sysctlbyname(name: *const std::ffi::c_char, old: *mut std::ffi::c_void, old_len: *mut usize, new: *const std::ffi::c_void, new_len: usize) -> i32;
    }
    let mut bytes = 0_u64;
    let mut len = std::mem::size_of::<u64>();
    // SAFETY: reads one 64-bit number into a variable of that size.
    let result = unsafe { sysctlbyname(c"hw.memsize".as_ptr(), &mut bytes as *mut u64 as *mut _, &mut len, std::ptr::null(), 0) };
    (result == 0 && bytes > 0).then_some(bytes)
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn total_memory() -> Option<u64> {
    None
}

/// Opens the folder holding `path` in File Explorer or Finder, with the file selected.
pub fn reveal(path: &std::path::Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Explorer reads its own command line, so the path is quoted by hand.
        let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{}\"", path.display())).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
    }
}

/// A moment as the computer's clock shows it: "12 Nov 2026, 14:03".
#[cfg(windows)]
pub fn local_time(when: std::time::SystemTime) -> Option<String> {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    // FILETIME counts 100 ns steps from 1601.
    let since_1970 = when.duration_since(std::time::UNIX_EPOCH).ok()?;
    let ticks = since_1970.as_nanos() as u64 / 100 + 116_444_736_000_000_000;
    let file_time = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let (mut utc, mut local) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    // SAFETY: converts between structs owned here.
    unsafe {
        FileTimeToSystemTime(&file_time, &mut utc).ok()?;
        SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).ok()?;
    }
    let month = crate::info::MONTHS.get(local.wMonth.checked_sub(1)? as usize)?;
    Some(format!("{} {month} {}, {:02}:{:02}", local.wDay, local.wYear, local.wHour, local.wMinute))
}

/// A moment as the computer's clock shows it: "12 Nov 2026, 14:03".
#[cfg(target_os = "macos")]
pub fn local_time(when: std::time::SystemTime) -> Option<String> {
    #[repr(C)]
    struct Tm {
        sec: i32,
        min: i32,
        hour: i32,
        mday: i32,
        mon: i32,
        year: i32,
        wday: i32,
        yday: i32,
        isdst: i32,
        gmtoff: i64,
        zone: *const std::ffi::c_char,
    }
    extern "C" {
        fn localtime_r(time: *const i64, out: *mut Tm) -> *mut Tm;
    }
    let secs = when.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    let mut tm = Tm { sec: 0, min: 0, hour: 0, mday: 0, mon: 0, year: 0, wday: 0, yday: 0, isdst: 0, gmtoff: 0, zone: std::ptr::null() };
    // SAFETY: fills in a struct laid out as the C library's `struct tm`.
    if unsafe { localtime_r(&secs, &mut tm) }.is_null() {
        return None;
    }
    let month = crate::info::MONTHS.get(tm.mon as usize)?;
    Some(format!("{} {month} {}, {:02}:{:02}", tm.mday, tm.year + 1900, tm.hour, tm.min))
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn local_time(_when: std::time::SystemTime) -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
pub const TRASH: &str = "Trash";
#[cfg(not(target_os = "macos"))]
pub const TRASH: &str = "Recycle Bin";

/// A shortcut as written on this system: the key egui calls "command" is Ctrl on
/// Windows and Cmd on macOS.
pub fn keys(text: &str) -> String {
    #[cfg(target_os = "macos")]
    return text.replace("Ctrl", "Cmd");
    #[cfg(not(target_os = "macos"))]
    text.to_string()
}
