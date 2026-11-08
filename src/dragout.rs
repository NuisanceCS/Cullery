//! Dragging photos out of Cullery into other apps — Lightroom, LightCraft, File
//! Explorer, anything that accepts dropped files.
//!
//! The Windows shell builds the drag data from the file paths, exactly as File Explorer
//! does when you drag from it, so every app sees an ordinary multi-file drop (one
//! import for the whole group), with the usual drag image.

use std::path::PathBuf;

/// Drags `paths` out of the window `hwnd` (as a copy). Returns once the user drops
/// or cancels; the window doesn't redraw in the meantime.
#[cfg(windows)]
pub fn drag_files(hwnd: isize, paths: &[PathBuf]) -> Result<(), String> {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::System::Ole::{IDropSource, DROPEFFECT_COPY, DROPEFFECT_LINK};
    use windows::Win32::UI::Shell::SHDoDragDrop;
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_LBUTTONUP};

    let data = shell_data_object(paths)?;
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: called on the UI thread, where winit has set up OLE. SHDoDragDrop runs
    // the drag until the drop or cancel; with no drop source of our own, the shell
    // supplies the standard one.
    let result = unsafe { SHDoDragDrop(Some(hwnd), &data, None::<&IDropSource>, DROPEFFECT_COPY | DROPEFFECT_LINK) };
    // The drag swallowed the mouse-up, so the window still thinks the button is held.
    // Tell it the button came up.
    // SAFETY: posts a message to our own window.
    let _ = unsafe { PostMessageW(Some(hwnd), WM_LBUTTONUP, WPARAM(0), LPARAM(0)) };
    result.map(|_| ()).map_err(|e| e.message().to_string())
}

/// The shell's drag data for these files: the same thing File Explorer builds.
#[cfg(windows)]
fn shell_data_object(paths: &[PathBuf]) -> Result<windows::Win32::System::Com::IDataObject, String> {
    use windows::core::HSTRING;
    use windows::Win32::System::Com::{IBindCtx, IDataObject};
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows::Win32::UI::Shell::{
        ILFree, IShellItemArray, SHCreateShellItemArrayFromIDLists, SHParseDisplayName, BHID_DataObject,
    };

    // SAFETY: plain shell calls; every ID list from SHParseDisplayName is freed with
    // ILFree once the data object (which keeps its own copies) has been made.
    unsafe {
        let mut pidls: Vec<*mut ITEMIDLIST> = Vec::new();
        for path in paths {
            let mut pidl = std::ptr::null_mut();
            let name = HSTRING::from(path.as_os_str());
            if SHParseDisplayName(&name, None::<&IBindCtx>, &mut pidl, 0, None).is_ok() && !pidl.is_null() {
                pidls.push(pidl);
            }
        }
        if pidls.is_empty() {
            return Err("None of the files could be found".into());
        }
        let ids: Vec<*const ITEMIDLIST> = pidls.iter().map(|p| *p as *const ITEMIDLIST).collect();
        let data = (|| -> windows::core::Result<IDataObject> {
            let items: IShellItemArray = SHCreateShellItemArrayFromIDLists(&ids)?;
            items.BindToHandler(None::<&IBindCtx>, &BHID_DataObject)
        })();
        for pidl in pidls {
            ILFree(Some(pidl as *const ITEMIDLIST));
        }
        data.map_err(|e| e.message().to_string())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// Windows builds drag data for real files in two different folders, holding the
    /// standard file-list format other apps read on drop.
    #[test]
    fn builds_shell_drag_data_for_files() {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
        use windows::Win32::System::Ole::CF_HDROP;

        let base = std::env::temp_dir().join(format!("cullery-dragout-test-{}", std::process::id()));
        let (a, b) = (base.join("a"), base.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let files = vec![a.join("IMG_0001.CR3"), b.join("IMG_0002.CR3")];
        for f in &files {
            std::fs::write(f, b"x").unwrap();
        }

        // SAFETY: initializes COM for this test thread.
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let data = shell_data_object(&files);
        let has_file_list = data.as_ref().map(|d| {
            let format = FORMATETC {
                cfFormat: CF_HDROP.0,
                ptd: std::ptr::null_mut(),
                dwAspect: DVASPECT_CONTENT.0,
                lindex: -1,
                tymed: TYMED_HGLOBAL.0 as u32,
            };
            // SAFETY: asks the data object whether it can supply this format.
            unsafe { d.QueryGetData(&format) }.is_ok()
        });
        std::fs::remove_dir_all(&base).unwrap();
        assert_eq!(has_file_list, Ok(true));
        assert!(shell_data_object(&[base.join("missing.CR3")]).is_err());
    }
}

#[cfg(not(windows))]
pub fn drag_files(_hwnd: isize, _paths: &[PathBuf]) -> Result<(), String> {
    Err("Dragging photos out is only supported on Windows".into())
}
