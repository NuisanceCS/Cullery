//! Plays a video clip inside the viewer, sound included, through the system's own
//! player: Media Foundation's Media Engine on Windows, AVFoundation's AVPlayer on macOS.
//! Cullery asks it for the newest frame each time the window draws.

use std::path::Path;

use eframe::egui::{Color32, ColorImage};

/// Whether the system player can play this clip inside Cullery (RAW video can't).
pub fn can_play(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    match ext.as_str() {
        "mp4" | "mov" | "m4v" => true,
        // AVCHD: Windows plays it, macOS's player doesn't.
        "mts" | "m2ts" => cfg!(windows),
        _ => false,
    }
}

/// `width` × `height` BGRA pixels, rows `stride` bytes apart, as an opaque picture.
///
/// # Safety
/// `data` must point to at least `stride * height` readable bytes.
#[allow(dead_code)]
unsafe fn bgra_to_image(data: *const u8, width: usize, height: usize, stride: usize) -> ColorImage {
    let mut pixels = Vec::with_capacity(width * height);
    for y in 0..height {
        let row = std::slice::from_raw_parts(data.add(y * stride), width * 4);
        pixels.extend(row.chunks_exact(4).map(|p| Color32::from_rgb(p[2], p[1], p[0])));
    }
    ColorImage { size: [width, height], pixels, ..Default::default() }
}

/// The largest size with `native`'s shape that fits in `max`, never bigger than `native`.
#[allow(dead_code)]
fn fit(native: [u32; 2], max: [u32; 2]) -> [u32; 2] {
    let scale = (max[0] as f64 / native[0] as f64).min(max[1] as f64 / native[1] as f64).min(1.0);
    [((native[0] as f64 * scale).round() as u32).max(1), ((native[1] as f64 * scale).round() as u32).max(1)]
}

pub use imp::Player;

#[cfg(windows)]
mod imp {
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use eframe::egui::ColorImage;
    use windows::core::{implement, BSTR};
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, IWICBitmap, IWICImagingFactory, WICBitmapCacheOnLoad, WICBitmapLockRead,
        WICRect,
    };
    use windows::Win32::Media::MediaFoundation::{
        CLSID_MFMediaEngineClassFactory, IMFAttributes, IMFMediaEngine, IMFMediaEngineClassFactory, IMFMediaEngineNotify,
        IMFMediaEngineNotify_Impl, MFCreateAttributes, MFShutdown, MFStartup, MF_MEDIA_ENGINE_CALLBACK, MF_MEDIA_ENGINE_EVENT_ERROR,
        MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT, MFSTARTUP_FULL, MF_VERSION,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};

    /// Hears from the Media Engine, which reports from its own threads.
    #[implement(IMFMediaEngineNotify)]
    struct Notify {
        failed: Arc<AtomicBool>,
    }

    impl IMFMediaEngineNotify_Impl for Notify_Impl {
        fn EventNotify(&self, event: u32, _param1: usize, _param2: u32) -> windows::core::Result<()> {
            if event == MF_MEDIA_ENGINE_EVENT_ERROR.0 as u32 {
                self.failed.store(true, Ordering::SeqCst);
            }
            Ok(())
        }
    }

    pub struct Player {
        engine: IMFMediaEngine,
        wic: IWICImagingFactory,
        /// Where frames are copied to, and its size.
        bitmap: Option<(IWICBitmap, [u32; 2])>,
        /// The time stamp of the last frame handed out.
        last_frame: Option<i64>,
        failed: Arc<AtomicBool>,
    }

    impl Player {
        pub fn open(path: &Path) -> Result<Self, String> {
            let failed = |e: windows::core::Error| format!("Windows couldn't play this clip: {}", e.message());
            // SAFETY: COM and Media Foundation calls with valid arguments; the engine and
            // everything it uses are owned by the returned Player and shut down on drop.
            unsafe {
                // The window's thread is already set up for COM; this is a no-op there.
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(failed)?;
                let result = (|| {
                    let factory: IMFMediaEngineClassFactory = CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_INPROC_SERVER)?;
                    let mut attributes: Option<IMFAttributes> = None;
                    MFCreateAttributes(&mut attributes, 2)?;
                    let attributes = attributes.ok_or_else(windows::core::Error::empty)?;
                    let failed_flag = Arc::new(AtomicBool::new(false));
                    let notify: IMFMediaEngineNotify = Notify { failed: failed_flag.clone() }.into();
                    attributes.SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &notify)?;
                    // No window of its own: frames are copied out, as BGRA.
                    attributes.SetUINT32(&MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32)?;
                    let engine = factory.CreateInstance(0, &attributes)?;
                    engine.SetSource(&BSTR::from(path.to_string_lossy().as_ref()))?;
                    let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
                    Ok(Self { engine, wic, bitmap: None, last_frame: None, failed: failed_flag })
                })();
                if result.is_err() {
                    let _ = MFShutdown();
                }
                result.map_err(failed)
            }
        }

        pub fn play(&self) {
            // SAFETY: plain Media Engine calls.
            unsafe {
                // At the end, start over.
                if self.engine.IsEnded().as_bool() {
                    let _ = self.engine.SetCurrentTime(0.0);
                }
                let _ = self.engine.Play();
            }
        }

        pub fn pause(&self) {
            // SAFETY: a plain Media Engine call.
            let _ = unsafe { self.engine.Pause() };
        }

        pub fn is_playing(&self) -> bool {
            // SAFETY: plain Media Engine calls.
            unsafe { !self.engine.IsPaused().as_bool() && !self.engine.IsEnded().as_bool() }
        }

        /// Seconds into the clip.
        pub fn position(&self) -> f64 {
            // SAFETY: a plain Media Engine call.
            unsafe { self.engine.GetCurrentTime() }
        }

        /// The clip's length in seconds, once it's known.
        pub fn duration(&self) -> Option<f64> {
            // SAFETY: a plain Media Engine call.
            let d = unsafe { self.engine.GetDuration() };
            (d.is_finite() && d > 0.0).then_some(d)
        }

        pub fn seek(&self, seconds: f64) {
            // SAFETY: a plain Media Engine call.
            let _ = unsafe { self.engine.SetCurrentTime(seconds.max(0.0)) };
        }

        pub fn set_muted(&self, muted: bool) {
            // SAFETY: a plain Media Engine call.
            let _ = unsafe { self.engine.SetMuted(muted) };
        }

        /// From 0 (silent) to 1 (full).
        pub fn set_volume(&self, volume: f32) {
            // SAFETY: a plain Media Engine call.
            let _ = unsafe { self.engine.SetVolume(volume.clamp(0.0, 1.0) as f64) };
        }

        pub fn failed(&self) -> bool {
            self.failed.load(Ordering::SeqCst)
        }

        /// The newest frame, if it's changed since the last call, sized to fit `max`.
        pub fn frame(&mut self, max: [u32; 2]) -> Option<ColorImage> {
            // SAFETY: Media Engine and WIC calls on objects this Player owns; the locked
            // bitmap's data pointer is only read within the lock's lifetime.
            unsafe {
                let stamp = self.engine.OnVideoStreamTick().ok()?;
                if self.last_frame == Some(stamp) {
                    return None;
                }
                let (mut w, mut h) = (0, 0);
                self.engine.GetNativeVideoSize(Some(&mut w), Some(&mut h)).ok()?;
                if w == 0 || h == 0 {
                    return None;
                }
                let size = super::fit([w, h], max);
                if self.bitmap.as_ref().is_none_or(|(_, s)| *s != size) {
                    let bitmap = self.wic.CreateBitmap(size[0], size[1], &GUID_WICPixelFormat32bppBGRA, WICBitmapCacheOnLoad).ok()?;
                    self.bitmap = Some((bitmap, size));
                }
                let (bitmap, _) = self.bitmap.as_ref()?;
                let target = RECT { left: 0, top: 0, right: size[0] as i32, bottom: size[1] as i32 };
                self.engine.TransferVideoFrame(bitmap, None, &target, None).ok()?;
                self.last_frame = Some(stamp);

                let area = WICRect { X: 0, Y: 0, Width: size[0] as i32, Height: size[1] as i32 };
                let lock = bitmap.Lock(&area, WICBitmapLockRead.0 as u32).ok()?;
                let stride = lock.GetStride().ok()? as usize;
                let (mut len, mut data) = (0, std::ptr::null_mut());
                lock.GetDataPointer(&mut len, &mut data).ok()?;
                if data.is_null() || (len as usize) < stride * size[1] as usize {
                    return None;
                }
                Some(super::bgra_to_image(data, size[0] as usize, size[1] as usize, stride))
            }
        }
    }

    impl Drop for Player {
        fn drop(&mut self) {
            // SAFETY: shuts down the engine this Player started, then Media Foundation.
            unsafe {
                let _ = self.engine.Shutdown();
                let _ = MFShutdown();
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;
    use std::path::Path;
    use std::ptr::null_mut;

    use eframe::egui::ColorImage;
    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::msg_send;
    use objc2_foundation::{NSString, NSURL};

    use crate::video::CMTime;

    #[link(name = "AVFoundation", kind = "framework")]
    #[link(name = "CoreMedia", kind = "framework")]
    #[link(name = "CoreVideo", kind = "framework")]
    #[link(name = "QuartzCore", kind = "framework")]
    extern "C" {
        static kCVPixelBufferPixelFormatTypeKey: *const c_void;
        fn CACurrentMediaTime() -> f64;
        fn CMTimeGetSeconds(time: CMTime) -> f64;
        fn CMTimeMakeWithSeconds(seconds: f64, timescale: i32) -> CMTime;
        fn CVPixelBufferLockBaseAddress(buffer: *mut c_void, flags: u64) -> i32;
        fn CVPixelBufferUnlockBaseAddress(buffer: *mut c_void, flags: u64) -> i32;
        fn CVPixelBufferGetBaseAddress(buffer: *mut c_void) -> *mut c_void;
        fn CVPixelBufferGetBytesPerRow(buffer: *mut c_void) -> usize;
        fn CVPixelBufferGetWidth(buffer: *mut c_void) -> usize;
        fn CVPixelBufferGetHeight(buffer: *mut c_void) -> usize;
        fn CVBufferRelease(buffer: *mut c_void);
    }

    /// 'BGRA', the pixel format frames come out in.
    const BGRA: u32 = u32::from_be_bytes(*b"BGRA");
    const LOCK_READ_ONLY: u64 = 1;
    /// AVPlayerItemStatusFailed.
    const STATUS_FAILED: isize = 2;

    pub struct Player {
        player: Retained<AnyObject>,
        item: Retained<AnyObject>,
        output: Retained<AnyObject>,
    }

    fn class(name: &std::ffi::CStr) -> Result<&'static AnyClass, String> {
        AnyClass::get(name).ok_or_else(|| "macOS couldn't play this clip".to_string())
    }

    impl Player {
        pub fn open(path: &Path) -> Result<Self, String> {
            let failed = || "macOS couldn't play this clip".to_string();
            let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
            // SAFETY: AVFoundation calls with the argument types they declare; every object
            // kept is retained by the Player.
            unsafe {
                let item: Option<Retained<AnyObject>> = msg_send![class(c"AVPlayerItem")?, playerItemWithURL: &*url];
                let item = item.ok_or_else(failed)?;
                let player: Option<Retained<AnyObject>> = msg_send![class(c"AVPlayer")?, playerWithPlayerItem: &*item];
                let player = player.ok_or_else(failed)?;

                let number: Option<Retained<AnyObject>> = msg_send![class(c"NSNumber")?, numberWithUnsignedInt: BGRA];
                let number = number.ok_or_else(failed)?;
                let key = &*(kCVPixelBufferPixelFormatTypeKey as *const AnyObject);
                let attributes: Option<Retained<AnyObject>> = msg_send![class(c"NSDictionary")?, dictionaryWithObject: &*number, forKey: key];
                let attributes = attributes.ok_or_else(failed)?;
                let allocated: Allocated<AnyObject> = msg_send![class(c"AVPlayerItemVideoOutput")?, alloc];
                let output: Option<Retained<AnyObject>> = msg_send![allocated, initWithPixelBufferAttributes: &*attributes];
                let output = output.ok_or_else(failed)?;
                let _: () = msg_send![&*item, addOutput: &*output];
                Ok(Self { player, item, output })
            }
        }

        pub fn play(&self) {
            // At the end, start over.
            if let Some(d) = self.duration() {
                if self.position() >= d - 0.05 {
                    self.seek(0.0);
                }
            }
            // SAFETY: a plain AVPlayer call.
            let _: () = unsafe { msg_send![&*self.player, play] };
        }

        pub fn pause(&self) {
            // SAFETY: a plain AVPlayer call.
            let _: () = unsafe { msg_send![&*self.player, pause] };
        }

        pub fn is_playing(&self) -> bool {
            // SAFETY: a plain AVPlayer call.
            let rate: f32 = unsafe { msg_send![&*self.player, rate] };
            rate != 0.0
        }

        pub fn position(&self) -> f64 {
            // SAFETY: plain AVPlayer and CoreMedia calls.
            unsafe {
                let time: CMTime = msg_send![&*self.player, currentTime];
                let s = CMTimeGetSeconds(time);
                if s.is_finite() { s } else { 0.0 }
            }
        }

        pub fn duration(&self) -> Option<f64> {
            // SAFETY: plain AVPlayerItem and CoreMedia calls.
            let d = unsafe {
                let time: CMTime = msg_send![&*self.item, duration];
                CMTimeGetSeconds(time)
            };
            (d.is_finite() && d > 0.0).then_some(d)
        }

        pub fn seek(&self, seconds: f64) {
            // SAFETY: plain AVPlayer and CoreMedia calls.
            unsafe {
                let to = CMTimeMakeWithSeconds(seconds.max(0.0), 600);
                let exact = CMTime { value: 0, timescale: 1, flags: 1, epoch: 0 };
                let _: () = msg_send![&*self.player, seekToTime: to, toleranceBefore: exact, toleranceAfter: exact];
            }
        }

        pub fn set_muted(&self, muted: bool) {
            // SAFETY: a plain AVPlayer call.
            let _: () = unsafe { msg_send![&*self.player, setMuted: muted] };
        }

        /// From 0 (silent) to 1 (full).
        pub fn set_volume(&self, volume: f32) {
            // SAFETY: a plain AVPlayer call.
            let _: () = unsafe { msg_send![&*self.player, setVolume: volume.clamp(0.0, 1.0)] };
        }

        pub fn failed(&self) -> bool {
            // SAFETY: a plain AVPlayerItem call.
            let status: isize = unsafe { msg_send![&*self.item, status] };
            status == STATUS_FAILED
        }

        /// The newest frame, if there's a new one, sized to fit `max`.
        pub fn frame(&mut self, max: [u32; 2]) -> Option<ColorImage> {
            // SAFETY: the pixel buffer is owned here ("copy"), locked while read, then
            // released.
            unsafe {
                let now: CMTime = msg_send![&*self.output, itemTimeForHostTime: CACurrentMediaTime()];
                let new: bool = msg_send![&*self.output, hasNewPixelBufferForItemTime: now];
                if !new {
                    return None;
                }
                let buffer: *mut c_void = msg_send![&*self.output, copyPixelBufferForItemTime: now, itemTimeForDisplay: null_mut::<CMTime>()];
                if buffer.is_null() {
                    return None;
                }
                CVPixelBufferLockBaseAddress(buffer, LOCK_READ_ONLY);
                let data = CVPixelBufferGetBaseAddress(buffer) as *const u8;
                let (w, h) = (CVPixelBufferGetWidth(buffer), CVPixelBufferGetHeight(buffer));
                let stride = CVPixelBufferGetBytesPerRow(buffer);
                let image = (!data.is_null() && w > 0 && h > 0).then(|| super::bgra_to_image(data, w, h, stride));
                CVPixelBufferUnlockBaseAddress(buffer, LOCK_READ_ONLY);
                CVBufferRelease(buffer);
                let image = image?;
                // Scale down big clips to what the window shows.
                let size = super::fit([w as u32, h as u32], max);
                if size == [w as u32, h as u32] {
                    return Some(image);
                }
                let rgba: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
                let buffer = image::RgbaImage::from_raw(w as u32, h as u32, rgba)?;
                let small = image::imageops::resize(&buffer, size[0], size[1], image::imageops::FilterType::Triangle);
                Some(ColorImage::from_rgba_unmultiplied([size[0] as usize, size[1] as usize], small.as_raw()))
            }
        }
    }

    impl Drop for Player {
        fn drop(&mut self) {
            self.pause();
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod imp {
    use std::path::Path;

    use eframe::egui::ColorImage;

    pub struct Player;

    impl Player {
        pub fn open(_path: &Path) -> Result<Self, String> {
            Err("Clips can't play inside Cullery on this system".into())
        }
        pub fn play(&self) {}
        pub fn pause(&self) {}
        pub fn is_playing(&self) -> bool {
            false
        }
        pub fn position(&self) -> f64 {
            0.0
        }
        pub fn duration(&self) -> Option<f64> {
            None
        }
        pub fn seek(&self, _seconds: f64) {}
        pub fn set_muted(&self, _muted: bool) {}
        pub fn set_volume(&self, _volume: f32) {}
        pub fn failed(&self) -> bool {
            true
        }
        pub fn frame(&mut self, _max: [u32; 2]) -> Option<ColorImage> {
            None
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// Plays a real clip (muted) and gets frames out of it.
    #[test]
    fn plays_a_clip() {
        let path = std::env::var_os("CULLERY_VIDEO_SAMPLE").map(std::path::PathBuf::from).unwrap_or_else(|| {
            r"C:\Windows\SystemApps\Microsoft.Windows.CloudExperienceHost_cw5n1h2txyewy\media\oobe-intro.mp4".into()
        });
        if !path.exists() {
            println!("no sample video on this PC; skipped");
            return;
        }
        let mut player = Player::open(&path).unwrap();
        player.set_volume(0.0);
        player.set_muted(true);
        player.play();
        let mut frames = 0;
        let mut size = [0, 0];
        let started = std::time::Instant::now();
        while started.elapsed().as_secs_f64() < 3.0 {
            if let Some(image) = player.frame([640, 640]) {
                frames += 1;
                size = image.size;
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        println!("{frames} frames at {size:?}, at {:.2}s of {:?}", player.position(), player.duration());
        assert!(!player.failed());
        assert!(frames > 10, "frames kept coming");
        assert!(size[0] > 0 && size[0] <= 640 && size[1] <= 640);
        assert!(player.position() > 1.0, "time moved on");
    }
}
