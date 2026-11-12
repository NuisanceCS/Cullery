//! Decoding through Windows Imaging Component (WIC) — the system decoders the Photos
//! app uses. Cullery uses it for HEIC/HEIF, which needs Microsoft's free "HEIF Image
//! Extensions" (and "HEVC Video Extensions") from the Store; without them, decoding
//! fails and the photo shows "No preview". On macOS the same job goes to Apple's ImageIO.

use std::path::Path;

/// Error when Windows has no decoder for the file's format at all (for HEIC: the HEIF
/// Image Extensions isn't installed).
pub const NO_READER: &str = "No Windows reader for this image type";
/// Error prefix when Windows has a decoder but it failed (for HEIC usually because
/// HEVC Video Extensions is missing, or the file is damaged).
pub const DECODE_FAILED: &str = "Windows couldn't decode this image: ";

/// A decoded image: RGBA pixels, size, the photo's full size, and its EXIF orientation.
pub struct Decoded {
    pub rgba: Vec<u8>,
    pub size: [u32; 2],
    pub original: [u32; 2],
    pub orientation: u32,
}

/// Decodes `path`, scaled down (by Windows) to fit within `max` before rotation.
#[cfg(windows)]
pub fn decode(path: &Path, max: [u32; 2]) -> Result<Decoded, String> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::GENERIC_READ;
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_WICPixelFormat32bppRGBA, IWICBitmapSource, IWICImagingFactory, IWICPalette,
        WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom,
        WICDecodeMetadataCacheOnDemand,
    };
    use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PROPVARIANT};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
    use windows::Win32::System::Variant::VT_UI2;

    let err = |e: windows::core::Error| format!("{DECODE_FAILED}{}", e.message());
    // SAFETY: COM calls on this thread (initialized here; a decode worker thread may
    // already be initialized, which is fine). Buffers passed to CopyPixels are sized
    // exactly stride × height.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).map_err(err)?;
        let name = HSTRING::from(path.as_os_str());
        let decoder = match factory.CreateDecoderFromFilename(
            PCWSTR(name.as_ptr()),
            None,
            GENERIC_READ,
            WICDecodeMetadataCacheOnDemand,
        ) {
            Ok(d) => d,
            // No decoder took the file. If Windows has no HEIF decoder at all, the
            // reader is missing; otherwise this file is the problem.
            Err(e) => {
                let is_heif = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("heic") || e.eq_ignore_ascii_case("heif"));
                return Err(if is_heif && !has_heif_reader(&factory) { NO_READER.to_string() } else { err(e) });
            }
        };
        let frame = decoder.GetFrame(0).map_err(err)?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).map_err(err)?;

        let mut orientation = 1;
        if let Ok(reader) = frame.GetMetadataQueryReader() {
            let mut value = PROPVARIANT::default();
            if reader.GetMetadataByName(&HSTRING::from("System.Photo.Orientation"), &mut value).is_ok()
                && value.Anonymous.Anonymous.vt == VT_UI2
            {
                orientation = value.Anonymous.Anonymous.Anonymous.uiVal as u32;
            }
            let _ = PropVariantClear(&mut value);
        }

        // Fit within `max` (in stored orientation) without enlarging.
        let sideways = (5..=8).contains(&orientation);
        let [mw, mh] = if sideways { [max[1], max[0]] } else { max };
        let scale = (mw as f64 / w as f64).min(mh as f64 / h as f64).min(1.0);
        let (tw, th) = (((w as f64 * scale).round() as u32).max(1), ((h as f64 * scale).round() as u32).max(1));

        let mut source: IWICBitmapSource = frame.into();
        if (tw, th) != (w, h) {
            let scaler = factory.CreateBitmapScaler().map_err(err)?;
            scaler.Initialize(&source, tw, th, WICBitmapInterpolationModeFant).map_err(err)?;
            source = scaler.into();
        }
        let converter = factory.CreateFormatConverter().map_err(err)?;
        converter
            .Initialize(
                &source,
                &GUID_WICPixelFormat32bppRGBA,
                WICBitmapDitherTypeNone,
                None::<&IWICPalette>,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
            .map_err(err)?;
        let stride = tw * 4;
        let mut rgba = vec![0u8; (stride * th) as usize];
        converter.CopyPixels(std::ptr::null(), stride, &mut rgba).map_err(err)?;

        let original = if sideways { [h, w] } else { [w, h] };
        Ok(Decoded { rgba, size: [tw, th], original, orientation })
    }
}

/// Whether Windows has a HEIF decoder (the HEIF Image Extensions).
#[cfg(windows)]
fn has_heif_reader(factory: &windows::Win32::Graphics::Imaging::IWICImagingFactory) -> bool {
    use windows::Win32::Graphics::Imaging::GUID_ContainerFormatHeif;
    // SAFETY: asks the factory for a decoder by container format.
    unsafe { factory.CreateDecoder(&GUID_ContainerFormatHeif, std::ptr::null()).is_ok() }
}

/// Decodes `path` with Apple's ImageIO, scaled down to fit within `max` before rotation.
#[cfg(target_os = "macos")]
pub fn decode(path: &Path, max: [u32; 2]) -> Result<Decoded, String> {
    mac::decode(path, max)
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn decode(_path: &Path, _max: [u32; 2]) -> Result<Decoded, String> {
    Err("HEIC photos can't be opened on this system".into())
}

#[cfg(target_os = "macos")]
#[allow(non_upper_case_globals, non_snake_case)]
pub mod mac {
    use std::ffi::c_void;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr::null;

    use super::Decoded;

    type CFTypeRef = *const c_void;

    #[repr(C)]
    struct CGRect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    }

    const K_CF_NUMBER_SINT32: isize = 3;
    const K_CG_IMAGE_ALPHA_PREMULTIPLIED_LAST: u32 = 1;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFAllocatorDefault: CFTypeRef;
        static kCFBooleanTrue: CFTypeRef;
        static kCFTypeDictionaryKeyCallBacks: u8;
        static kCFTypeDictionaryValueCallBacks: u8;
        fn CFRelease(cf: CFTypeRef);
        fn CFURLCreateFromFileSystemRepresentation(allocator: CFTypeRef, buffer: *const u8, length: isize, is_directory: u8) -> CFTypeRef;
        fn CFDictionaryCreate(allocator: CFTypeRef, keys: *const CFTypeRef, values: *const CFTypeRef, count: isize, key_callbacks: *const u8, value_callbacks: *const u8) -> CFTypeRef;
        fn CFDictionaryGetValue(dictionary: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
        fn CFNumberCreate(allocator: CFTypeRef, number_type: isize, value: *const c_void) -> CFTypeRef;
        fn CFNumberGetValue(number: CFTypeRef, number_type: isize, value: *mut c_void) -> u8;
    }

    #[link(name = "ImageIO", kind = "framework")]
    extern "C" {
        static kCGImagePropertyPixelWidth: CFTypeRef;
        static kCGImagePropertyPixelHeight: CFTypeRef;
        static kCGImagePropertyOrientation: CFTypeRef;
        static kCGImageSourceCreateThumbnailFromImageAlways: CFTypeRef;
        static kCGImageSourceThumbnailMaxPixelSize: CFTypeRef;
        fn CGImageSourceCreateWithURL(url: CFTypeRef, options: CFTypeRef) -> CFTypeRef;
        fn CGImageSourceCopyPropertiesAtIndex(source: CFTypeRef, index: usize, options: CFTypeRef) -> CFTypeRef;
        fn CGImageSourceCreateThumbnailAtIndex(source: CFTypeRef, index: usize, options: CFTypeRef) -> CFTypeRef;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        static kCGColorSpaceSRGB: CFTypeRef;
        fn CGColorSpaceCreateWithName(name: CFTypeRef) -> CFTypeRef;
        fn CGImageGetWidth(image: CFTypeRef) -> usize;
        fn CGImageGetHeight(image: CFTypeRef) -> usize;
        fn CGBitmapContextCreate(data: *mut c_void, width: usize, height: usize, bits_per_component: usize, bytes_per_row: usize, space: CFTypeRef, bitmap_info: u32) -> CFTypeRef;
        fn CGContextDrawImage(context: CFTypeRef, rect: CGRect, image: CFTypeRef);
    }

    /// A Core Foundation object this code owns, released when dropped.
    struct Owned(CFTypeRef);

    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: `owned` only wraps non-null objects from "Create" or "Copy" calls.
            unsafe { CFRelease(self.0) }
        }
    }

    fn owned(object: CFTypeRef) -> Result<Owned, String> {
        if object.is_null() {
            Err("macOS couldn't read this image".to_string())
        } else {
            Ok(Owned(object))
        }
    }

    pub fn decode(path: &Path, max: [u32; 2]) -> Result<Decoded, String> {
        let bytes = path.as_os_str().as_bytes();
        // SAFETY: every object passed along is checked non-null by `owned` and outlives
        // the call using it; `rgba` is exactly the size the bitmap context is told.
        unsafe {
            let url = owned(CFURLCreateFromFileSystemRepresentation(kCFAllocatorDefault, bytes.as_ptr(), bytes.len() as isize, 0))?;
            let source = owned(CGImageSourceCreateWithURL(url.0, null()))?;
            let properties = owned(CGImageSourceCopyPropertiesAtIndex(source.0, 0, null()))?;
            let number = |key: CFTypeRef| -> Option<u32> {
                let value = CFDictionaryGetValue(properties.0, key);
                let mut out = 0_i32;
                (!value.is_null() && CFNumberGetValue(value, K_CF_NUMBER_SINT32, &mut out as *mut i32 as *mut c_void) != 0)
                    .then_some(out as u32)
            };
            let too_odd = || "macOS couldn't read this image's size".to_string();
            let w = number(kCGImagePropertyPixelWidth).filter(|&n| n > 0).ok_or_else(too_odd)?;
            let h = number(kCGImagePropertyPixelHeight).filter(|&n| n > 0).ok_or_else(too_odd)?;
            let orientation = number(kCGImagePropertyOrientation).unwrap_or(1);

            // Fit within `max` (in stored orientation) without enlarging.
            let sideways = (5..=8).contains(&orientation);
            let [mw, mh] = if sideways { [max[1], max[0]] } else { max };
            let scale = (mw as f64 / w as f64).min(mh as f64 / h as f64).min(1.0);
            let long_edge = ((w.max(h) as f64 * scale).round() as i32).max(1);

            // Without the "with transform" option the picture stays in stored orientation.
            let limit = owned(CFNumberCreate(kCFAllocatorDefault, K_CF_NUMBER_SINT32, &long_edge as *const i32 as *const c_void))?;
            let keys = [kCGImageSourceCreateThumbnailFromImageAlways, kCGImageSourceThumbnailMaxPixelSize];
            let values = [kCFBooleanTrue, limit.0];
            let options = owned(CFDictionaryCreate(
                kCFAllocatorDefault,
                keys.as_ptr(),
                values.as_ptr(),
                2,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            ))?;
            let image = owned(CGImageSourceCreateThumbnailAtIndex(source.0, 0, options.0))?;
            let (rgba, size) = pixels(image.0)?;
            let original = if sideways { [h, w] } else { [w, h] };
            Ok(Decoded { rgba, size, original, orientation })
        }
    }

    /// A CGImage's pixels as straight (not premultiplied) RGBA, and its size.
    ///
    /// # Safety
    /// `image` must be a valid CGImage.
    pub unsafe fn pixels(image: CFTypeRef) -> Result<(Vec<u8>, [u32; 2]), String> {
        let (width, height) = (CGImageGetWidth(image), CGImageGetHeight(image));
        let mut rgba = vec![0_u8; width * height * 4];
        let space = owned(CGColorSpaceCreateWithName(kCGColorSpaceSRGB))?;
        let context = owned(CGBitmapContextCreate(
            rgba.as_mut_ptr() as *mut c_void,
            width,
            height,
            8,
            width * 4,
            space.0,
            K_CG_IMAGE_ALPHA_PREMULTIPLIED_LAST,
        ))?;
        CGContextDrawImage(context.0, CGRect { x: 0.0, y: 0.0, width: width as f64, height: height as f64 }, image);
        drop(context);

        // The bitmap is premultiplied; the rest of the app expects straight alpha.
        for pixel in rgba.chunks_exact_mut(4) {
            let alpha = pixel[3] as u32;
            if alpha != 0 && alpha != 255 {
                for channel in &mut pixel[..3] {
                    *channel = ((*channel as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
                }
            }
        }
        Ok((rgba, [width as u32, height as u32]))
    }

    /// Releases a Core Foundation object (a CGImage, say).
    ///
    /// # Safety
    /// `object` must be a non-null object the caller owns.
    pub unsafe fn release(object: CFTypeRef) {
        CFRelease(object)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// With a HEIC reader installed (as on this PC), a damaged HEIC reports a failed
    /// decode — not a missing reader, which would wrongly send the user to the Store
    /// for something they already have.
    #[test]
    fn damaged_heic_is_not_reported_as_missing_reader() {
        let path = std::env::temp_dir().join(format!("cullery-wic-bad-{}.heic", std::process::id()));
        std::fs::write(&path, b"definitely not a HEIC file").unwrap();
        let result = decode(&path, [100, 100]);
        std::fs::remove_file(&path).unwrap();
        let reason = result.err().expect("garbage shouldn't decode");
        // SAFETY: COM setup and a factory query.
        let has_reader = unsafe {
            use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory, IWICImagingFactory};
            use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap();
            has_heif_reader(&factory)
        };
        if has_reader {
            assert!(reason.starts_with(DECODE_FAILED), "got: {reason}");
        } else {
            assert_eq!(reason, NO_READER);
        }
    }

    /// Encodes a test image as HEIC with Windows' own encoder (when the HEIF extension
    /// is installed), then decodes it at reduced size.
    #[test]
    fn decodes_heic_through_windows() {
        use windows::core::HSTRING;
        use windows::Win32::Foundation::GENERIC_WRITE;
        use windows::Win32::Graphics::Imaging::{
            CLSID_WICImagingFactory, GUID_ContainerFormatHeif, GUID_WICPixelFormat24bppRGB, IWICImagingFactory,
            WICBitmapEncoderNoCache,
        };
        use windows::Win32::System::Com::StructuredStorage::IPropertyBag2;
        use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};

        let path = std::env::temp_dir().join(format!("cullery-wic-test-{}.heic", std::process::id()));
        let (w, h) = (640u32, 400u32);
        // SAFETY: COM encoding calls; the pixel buffer holds exactly w × h × 3 bytes.
        let encoded = unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            (|| -> windows::core::Result<()> {
                let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
                let stream = factory.CreateStream()?;
                stream.InitializeFromFilename(&HSTRING::from(path.as_os_str()), GENERIC_WRITE.0)?;
                let encoder = factory.CreateEncoder(&GUID_ContainerFormatHeif, std::ptr::null())?;
                encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
                let mut frame = None;
                let mut options: Option<IPropertyBag2> = None;
                encoder.CreateNewFrame(&mut frame, &mut options)?;
                let frame = frame.unwrap();
                frame.Initialize(options.as_ref())?;
                frame.SetSize(w, h)?;
                // Give the encoder a finished bitmap; it converts to the format it wants.
                let pixels: Vec<u8> = (0..w * h).flat_map(|i| [(i % w) as u8, (i / w) as u8, 128]).collect();
                let bitmap = factory.CreateBitmapFromMemory(w, h, &GUID_WICPixelFormat24bppRGB, w * 3, &pixels)?;
                frame.WriteSource(&bitmap, std::ptr::null())?;
                frame.Commit()?;
                encoder.Commit()
            })()
        };
        if let Err(e) = encoded {
            let _ = std::fs::remove_file(&path);
            println!("HEIC encoder not available here, skipping: {}", e.message());
            return;
        }
        let decoded = decode(&path, [320, 320]);
        std::fs::remove_file(&path).unwrap();
        let d = decoded.unwrap();
        assert_eq!(d.original, [w, h]);
        assert_eq!(d.size, [320, 200]);
        assert_eq!(d.rgba.len(), 320 * 200 * 4);
    }
}
