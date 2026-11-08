//! Decoding through Windows Imaging Component (WIC) — the system decoders the Photos
//! app uses. Cullery uses it for HEIC/HEIF, which needs Microsoft's free "HEIF Image
//! Extensions" (and "HEVC Video Extensions") from the Store; without them, decoding
//! fails and the photo shows "No preview".

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

#[cfg(not(windows))]
pub fn decode(_path: &Path, _max: [u32; 2]) -> Result<Decoded, String> {
    Err("HEIC needs Windows' image decoders".into())
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
