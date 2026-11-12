//! A still frame of a video clip, for its tile and the viewer. It comes from the system:
//! the thumbnails File Explorer shows on Windows, and AVFoundation on macOS. Playing a
//! clip is left to the system's video player.

use std::path::Path;

/// A frame of the clip at `path` as RGBA pixels and their size, fitting within `max`.
#[cfg(windows)]
pub fn frame(path: &Path, max: [u32; 2]) -> Result<(Vec<u8>, [u32; 2]), String> {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::{CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, DIB_RGB_COLORS, HGDIOBJ};
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
    use windows::Win32::UI::Shell::{IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_THUMBNAILONLY};

    let failed = |e: windows::core::Error| format!("Windows couldn't make a preview of this video: {}", e.message());
    // SAFETY: COM calls on this thread (initialized here; a decode worker may already be
    // initialized, which is fine). The bitmap is read into a buffer of exactly
    // width × height × 4 bytes, then deleted.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None).map_err(failed)?;
        let size = SIZE { cx: max[0].min(4096) as i32, cy: max[1].min(4096) as i32 };
        let bitmap = factory.GetImage(size, SIIGBF_THUMBNAILONLY).map_err(failed)?;
        let object = HGDIOBJ(bitmap.0);

        let mut info = BITMAP::default();
        let read = GetObjectW(object, std::mem::size_of::<BITMAP>() as i32, Some(&mut info as *mut BITMAP as *mut _));
        let (w, h) = (info.bmWidth, info.bmHeight.abs());
        if read == 0 || w <= 0 || h <= 0 {
            let _ = DeleteObject(object);
            return Err("Windows made an empty preview of this video".into());
        }
        let mut bgra = vec![0_u8; (w * h * 4) as usize];
        let dc = CreateCompatibleDC(None);
        let mut header = crate::dragimage::bitmap_info(w, h);
        let lines = GetDIBits(dc, bitmap, 0, h as u32, Some(bgra.as_mut_ptr() as *mut _), &mut header, DIB_RGB_COLORS);
        let _ = DeleteDC(dc);
        let _ = DeleteObject(object);
        if lines == 0 {
            return Err("Windows couldn't read the preview of this video".into());
        }

        // Video frames come without transparency; a bitmap whose alpha is all zero is opaque.
        let opaque = bgra.chunks_exact(4).all(|p| p[3] == 0);
        let mut rgba = Vec::with_capacity(bgra.len());
        for p in bgra.chunks_exact(4) {
            let alpha = if opaque { 255 } else { p[3] as u32 };
            let straight = |c: u8| if alpha == 0 || alpha == 255 { c } else { ((c as u32 * 255 + alpha / 2) / alpha).min(255) as u8 };
            rgba.extend_from_slice(&[straight(p[2]), straight(p[1]), straight(p[0]), alpha as u8]);
        }
        Ok((rgba, [w as u32, h as u32]))
    }
}

/// CoreMedia's time value, as AVFoundation passes it around.
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CMTime {
    pub value: i64,
    pub timescale: i32,
    pub flags: u32,
    pub epoch: i64,
}

// SAFETY: matches the C struct's layout and Objective-C type encoding.
#[cfg(target_os = "macos")]
unsafe impl objc2::encode::Encode for CMTime {
    const ENCODING: objc2::encode::Encoding = objc2::encode::Encoding::Struct(
        "?",
        &[i64::ENCODING, i32::ENCODING, u32::ENCODING, i64::ENCODING],
    );
}

// SAFETY: a pointer to the struct above.
#[cfg(target_os = "macos")]
unsafe impl objc2::encode::RefEncode for CMTime {
    const ENCODING_REF: objc2::encode::Encoding = objc2::encode::Encoding::Pointer(&<Self as objc2::encode::Encode>::ENCODING);
}

/// A frame of the clip at `path` as RGBA pixels and their size, fitting within `max`.
#[cfg(target_os = "macos")]
pub fn frame(path: &Path, max: [u32; 2]) -> Result<(Vec<u8>, [u32; 2]), String> {
    use std::ffi::c_void;
    use objc2::rc::autoreleasepool;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::msg_send;
    use objc2_foundation::{NSSize, NSString, NSURL};

    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {}

    let failed = || "macOS couldn't make a preview of this video".to_string();
    autoreleasepool(|_| {
        let asset_class = AnyClass::get(c"AVURLAsset").ok_or_else(failed)?;
        let generator_class = AnyClass::get(c"AVAssetImageGenerator").ok_or_else(failed)?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        // SAFETY: standard AVFoundation calls with the argument types they declare. The
        // returned CGImage is owned here ("copy") and released after its pixels are read.
        unsafe {
            let asset: *mut AnyObject = msg_send![asset_class, URLAssetWithURL: &*url, options: std::ptr::null::<AnyObject>()];
            if asset.is_null() {
                return Err(failed());
            }
            let generator: *mut AnyObject = msg_send![generator_class, assetImageGeneratorWithAsset: asset];
            if generator.is_null() {
                return Err(failed());
            }
            // Upright, as the clip is meant to be seen, and no bigger than needed.
            let _: () = msg_send![generator, setAppliesPreferredTrackTransform: true];
            let _: () = msg_send![generator, setMaximumSize: NSSize::new(max[0] as f64, max[1] as f64)];
            // One second in: the very first frame is often black.
            let at = CMTime { value: 1, timescale: 1, flags: 1, epoch: 0 };
            let image: *const c_void = msg_send![
                generator,
                copyCGImageAtTime: at,
                actualTime: std::ptr::null_mut::<CMTime>(),
                error: std::ptr::null_mut::<*mut AnyObject>()
            ];
            if image.is_null() {
                return Err(failed());
            }
            let result = crate::wic::mac::pixels(image);
            crate::wic::mac::release(image);
            result
        }
    })
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn frame(_path: &Path, _max: [u32; 2]) -> Result<(Vec<u8>, [u32; 2]), String> {
    Err("Video previews aren't supported on this system".into())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// A video that comes with Windows: its preview should be a real picture.
    /// Needs a PC with Windows' video features, which the build servers lack:
    /// cargo test --release windows_makes_a_frame -- --ignored --nocapture
    #[test]
    #[ignore]
    fn windows_makes_a_frame() {
        let path = std::env::var_os("CULLERY_VIDEO_SAMPLE").map(std::path::PathBuf::from).unwrap_or_else(|| {
            r"C:\Windows\SystemApps\Microsoft.Windows.CloudExperienceHost_cw5n1h2txyewy\media\oobe-intro.mp4".into()
        });
        let sample = path.as_path();
        if !sample.exists() {
            println!("no sample video on this PC; skipped");
            return;
        }
        let (rgba, [w, h]) = frame(sample, [640, 640]).unwrap();
        let distinct: std::collections::HashSet<&[u8]> = rgba.chunks_exact(4).collect();
        println!("frame {w}x{h}, {} distinct colors", distinct.len());
        assert!(w > 0 && h > 0 && w <= 640 && h <= 640);
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        assert!(rgba.chunks_exact(4).all(|p| p[3] == 255), "video frames are opaque");
    }
}
