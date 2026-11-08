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

#[cfg(not(windows))]
pub fn set_backdrop(_hwnd: isize, _mica: bool, _dark: bool) -> bool {
    false
}
