//! Window chrome on Windows: the title bar's light/dark mode and the Windows 11 Mica
//! backdrop (the wallpaper-tinted material File Explorer's top area uses).

/// Sets the title bar to dark or light and turns Mica on or off. Returns whether Mica
/// is showing: it needs Windows 11 22H2 or newer; elsewhere the call fails and the app
/// keeps drawing its own solid top bar.
#[cfg(windows)]
pub fn set_backdrop(hwnd: isize, mica: bool, dark: bool) -> bool {
    use windows::core::BOOL;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{
        DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMSBT_AUTO, DWMSBT_TABBEDWINDOW,
        DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
    };
    use windows::Win32::UI::Controls::MARGINS;

    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: DWM attribute calls on our own window, each passing a pointer to a local
    // of exactly the size given.
    unsafe {
        let dark = BOOL::from(dark);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const BOOL as *const core::ffi::c_void,
            std::mem::size_of::<BOOL>() as u32,
        );
        // "Mica Alt", the stronger wallpaper tint File Explorer's tab bar uses.
        let backdrop = if mica { DWMSBT_TABBEDWINDOW } else { DWMSBT_AUTO };
        let ok = DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &backdrop as *const _ as *const core::ffi::c_void,
            std::mem::size_of_val(&backdrop) as u32,
        )
        .is_ok();
        // Mica shows through wherever the app draws nothing (the top bar), so let the
        // backdrop extend under the whole window.
        let edge = if mica && ok { -1 } else { 0 };
        let margins = MARGINS { cxLeftWidth: edge, cxRightWidth: edge, cyTopHeight: edge, cyBottomHeight: edge };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
        mica && ok
    }
}

/// Gives the window Cullery's icon from the one built into the exe, at the sizes this
/// display uses for the title bar and the taskbar (Windows picks each from the icon
/// file's sizes, so they're sharp rather than scaled down from one picture).
#[cfg(windows)]
pub fn set_icons(hwnd: isize) {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
    use windows::Win32::UI::WindowsAndMessaging::{
        LoadImageW, SendMessageW, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_DEFAULTCOLOR, SM_CXICON,
        SM_CXSMICON, WM_SETICON,
    };
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: loads icon resource 1 (added by build.rs) from our own exe and hands
    // it to our own window; Windows keeps the icons for the life of the process.
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return };
        let dpi = GetDpiForWindow(hwnd).max(96);
        for (which, metric) in [(ICON_SMALL, SM_CXSMICON), (ICON_BIG, SM_CXICON)] {
            let size = GetSystemMetricsForDpi(metric, dpi);
            // MAKEINTRESOURCE(1)
            let name = PCWSTR(1 as *const u16);
            if let Ok(icon) = LoadImageW(Some(module.into()), name, IMAGE_ICON, size, size, LR_DEFAULTCOLOR) {
                SendMessageW(hwnd, WM_SETICON, Some(WPARAM(which as usize)), Some(LPARAM(icon.0 as isize)));
            }
        }
    }
}

#[cfg(not(windows))]
pub fn set_icons(_hwnd: isize) {}

/// Opens a link (here: a Microsoft Store page) with whatever Windows uses for it.
#[cfg(windows)]
pub fn open_link(url: &str) {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let (verb, target) = (HSTRING::from("open"), HSTRING::from(url));
    // SAFETY: asks the shell to open a fixed URL; no pointers outlive the call.
    unsafe {
        ShellExecuteW(None, &verb, &target, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

#[cfg(target_os = "macos")]
pub fn open_link(url: &str) {
    let _ = std::process::Command::new("open").arg(url).spawn();
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn open_link(_url: &str) {}

#[cfg(not(windows))]
pub fn set_backdrop(_hwnd: isize, _mica: bool, _dark: bool) -> bool {
    false
}
