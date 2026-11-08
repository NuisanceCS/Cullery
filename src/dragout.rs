//! Dragging photos out of Cullery into other apps â€” Lightroom, LightCraft, File
//! Explorer, anything that accepts dropped files.
//!
//! The Windows shell builds the drag data from the file paths, exactly as File Explorer
//! does when you drag from it, so every app sees an ordinary multi-file drop (one
//! import for the whole group), with the usual drag image.

use std::path::PathBuf;

use crate::dragimage::DragImage;

/// Drags `paths` out of the window `hwnd` (as a copy), with `image` under the cursor
/// (else the shell's usual one). Returns once the user drops or cancels; the window
/// doesn't redraw in the meantime.
#[cfg(windows)]
pub fn drag_files(hwnd: isize, paths: &[PathBuf], image: Option<&DragImage>) -> Result<(), String> {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::System::Ole::{IDropSource, DROPEFFECT_COPY, DROPEFFECT_LINK};
    use windows::Win32::UI::Shell::SHDoDragDrop;
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_LBUTTONUP};

    let data = shell_data_object(paths)?;
    if let Some(image) = image {
        // If it can't be set, the shell makes its usual drag image instead.
        let _ = set_drag_image(&data, image);
    }
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    // Over Cullery itself, the drag picture only shows if the window asks for it; the
    // window's own drop handling doesn't, so a cover that does sits on top meanwhile.
    let cover = image.and_then(|_| ImageCover::new(hwnd));
    // SAFETY: called on the UI thread, where winit has set up OLE. SHDoDragDrop runs
    // the drag until the drop or cancel; with no drop source of our own, the shell
    // supplies the standard one.
    let result = unsafe { SHDoDragDrop(Some(hwnd), &data, None::<&IDropSource>, DROPEFFECT_COPY | DROPEFFECT_LINK) };
    drop(cover);
    // The drag swallowed the mouse-up, so the window still thinks the button is held.
    // Tell it the button came up.
    // SAFETY: posts a message to our own window.
    let _ = unsafe { PostMessageW(Some(hwnd), WM_LBUTTONUP, WPARAM(0), LPARAM(0)) };
    result.map(|_| ()).map_err(|e| e.message().to_string())
}

/// An invisible child window over the whole of Cullery during a drag out. Windows
/// shows the drag picture over a window only when that window's drop target asks
/// for it; this one does (and accepts nothing: photos dropped back do nothing).
/// It never paints, so the window underneath stays as it was.
#[cfg(windows)]
struct ImageCover {
    hwnd: windows::Win32::Foundation::HWND,
}

#[cfg(windows)]
#[windows_core::implement(windows::Win32::System::Ole::IDropTarget)]
struct ShowDragImage {
    helper: windows::Win32::UI::Shell::IDropTargetHelper,
    hwnd: windows::Win32::Foundation::HWND,
}

#[cfg(windows)]
#[allow(non_snake_case)]
impl windows::Win32::System::Ole::IDropTarget_Impl for ShowDragImage_Impl {
    fn DragEnter(
        &self,
        data: windows::core::Ref<'_, windows::Win32::System::Com::IDataObject>,
        _keys: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
        pt: &windows::Win32::Foundation::POINTL,
        effect: *mut windows::Win32::System::Ole::DROPEFFECT,
    ) -> windows::core::Result<()> {
        use windows::Win32::System::Ole::DROPEFFECT_NONE;
        let point = windows::Win32::Foundation::POINT { x: pt.x, y: pt.y };
        // SAFETY: `effect` is the caller's out-parameter; the helper only reads `point`.
        unsafe {
            *effect = DROPEFFECT_NONE;
            if let Ok(data) = data.ok() {
                let _ = self.helper.DragEnter(self.hwnd, data, &point, DROPEFFECT_NONE);
            }
        }
        Ok(())
    }

    fn DragOver(
        &self,
        _keys: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
        pt: &windows::Win32::Foundation::POINTL,
        effect: *mut windows::Win32::System::Ole::DROPEFFECT,
    ) -> windows::core::Result<()> {
        use windows::Win32::System::Ole::DROPEFFECT_NONE;
        let point = windows::Win32::Foundation::POINT { x: pt.x, y: pt.y };
        // SAFETY: as in DragEnter.
        unsafe {
            *effect = DROPEFFECT_NONE;
            let _ = self.helper.DragOver(&point, DROPEFFECT_NONE);
        }
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        // SAFETY: tells the helper the drag left this window.
        let _ = unsafe { self.helper.DragLeave() };
        Ok(())
    }

    fn Drop(
        &self,
        data: windows::core::Ref<'_, windows::Win32::System::Com::IDataObject>,
        _keys: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
        pt: &windows::Win32::Foundation::POINTL,
        effect: *mut windows::Win32::System::Ole::DROPEFFECT,
    ) -> windows::core::Result<()> {
        use windows::Win32::System::Ole::DROPEFFECT_NONE;
        let point = windows::Win32::Foundation::POINT { x: pt.x, y: pt.y };
        // SAFETY: as in DragEnter.
        unsafe {
            *effect = DROPEFFECT_NONE;
            if let Ok(data) = data.ok() {
                let _ = self.helper.Drop(data, &point, DROPEFFECT_NONE);
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
impl ImageCover {
    fn new(parent: windows::Win32::Foundation::HWND) -> Option<Self> {
        use windows::core::w;
        use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
        use windows::Win32::Graphics::Gdi::ValidateRect;
        use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::System::Ole::{IDropTarget, RegisterDragDrop};
        use windows::Win32::UI::Shell::{CLSID_DragDropHelper, IDropTargetHelper};
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, GetClientRect, RegisterClassW, WINDOW_EX_STYLE,
            WM_ERASEBKGND, WM_PAINT, WNDCLASSW, WS_CHILD, WS_VISIBLE,
        };

        /// Paints nothing: the window underneath shows through unchanged.
        unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
            match msg {
                WM_ERASEBKGND => LRESULT(1),
                WM_PAINT => {
                    // SAFETY: marks our own window as painted.
                    let _ = unsafe { ValidateRect(Some(hwnd), None) };
                    LRESULT(0)
                }
                // SAFETY: default handling for our own window.
                _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
            }
        }

        // SAFETY: plain Win32 calls on the UI thread. The window class is registered
        // once (later attempts fail harmlessly); the child window is destroyed in Drop.
        unsafe {
            let instance = GetModuleHandleW(None).ok()?;
            let class = w!("CulleryDragCover");
            let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: instance.into(), lpszClassName: class, ..Default::default() };
            RegisterClassW(&wc);
            let mut rect = RECT::default();
            GetClientRect(parent, &mut rect).ok()?;
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                None,
                WS_CHILD | WS_VISIBLE,
                0,
                0,
                rect.right - rect.left,
                rect.bottom - rect.top,
                Some(parent),
                None,
                Some(instance.into()),
                None,
            )
            .ok()?;
            let cover = Self { hwnd };
            let helper: IDropTargetHelper = CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER).ok()?;
            let target: IDropTarget = ShowDragImage { helper, hwnd }.into();
            RegisterDragDrop(hwnd, &target).ok()?;
            Some(cover)
        }
    }
}

#[cfg(windows)]
impl Drop for ImageCover {
    fn drop(&mut self) {
        use windows::Win32::System::Ole::RevokeDragDrop;
        use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;
        // SAFETY: undoes `new` on our own child window (revoking a window that has no
        // drop target just fails).
        unsafe {
            let _ = RevokeDragDrop(self.hwnd);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// Attaches `image` to the drag data, for Windows to show under the cursor.
#[cfg(windows)]
fn set_drag_image(data: &windows::Win32::System::Com::IDataObject, image: &DragImage) -> windows::core::Result<()> {
    use windows::core::Interface;
    use windows::Win32::Foundation::{COLORREF, POINT, SIZE};
    use windows::Win32::Graphics::Gdi::{CreateDIBSection, DeleteObject, DIB_RGB_COLORS};
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    use windows::Win32::UI::Shell::{
        CLSID_DragDropHelper, IDragSourceHelper, IDragSourceHelper2, DSH_ALLOWDROPDESCRIPTIONTEXT, SHDRAGIMAGE,
    };

    let [w, h] = [image.size[0] as i32, image.size[1] as i32];
    let info = crate::dragimage::bitmap_info(w, h);
    // SAFETY: the bitmap's pixel buffer is exactly w*h*4 bytes, filled from `image`
    // (same size). On success the drag helper owns the bitmap; otherwise it's freed here.
    unsafe {
        let mut bits = std::ptr::null_mut();
        let bitmap = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
        if bits.is_null() || image.bgra.len() != (w * h * 4) as usize {
            let _ = DeleteObject(bitmap.into());
            return Err(windows::core::Error::from_win32());
        }
        std::ptr::copy_nonoverlapping(image.bgra.as_ptr(), bits as *mut u8, image.bgra.len());
        let helper: IDragSourceHelper = match CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER) {
            Ok(h) => h,
            Err(e) => {
                let _ = DeleteObject(bitmap.into());
                return Err(e);
            }
        };
        // Lets drop targets show "Copy to â€¦" under the stack, as in Explorer.
        if let Ok(helper2) = helper.cast::<IDragSourceHelper2>() {
            let _ = helper2.SetFlags(DSH_ALLOWDROPDESCRIPTIONTEXT.0 as u32);
        }
        let shdi = SHDRAGIMAGE {
            sizeDragImage: SIZE { cx: w, cy: h },
            ptOffset: POINT { x: image.hotspot[0], y: image.hotspot[1] },
            hbmpDragImage: bitmap,
            crColorKey: COLORREF(0xFFFF_FFFF),
        };
        let result = helper.InitializeFromBitmap(&shdi, data);
        if result.is_err() {
            let _ = DeleteObject(bitmap.into());
        }
        result
    }
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
pub fn drag_files(_hwnd: isize, _paths: &[PathBuf], _image: Option<&DragImage>) -> Result<(), String> {
    Err("Dragging photos out is only supported on Windows".into())
}
