//! Decodes JPGs on background threads so scrolling never waits on the disk.
//!
//! Four sizes per shot:
//! - `Preview`: the tiny thumbnail the camera stores inside the JPG. Only the first
//!   few KB of the file are read, so it shows almost instantly, even from an SD card.
//! - `Thumb`: a sharp grid thumbnail decoded from the full JPG at reduced scale.
//! - `Full`: the image sized to the viewer.
//! - `Original`: every pixel, for zooming in.
//!
//! RAW files (shots with no JPG) use the JPEGs the camera embedded in them instead;
//! see `raw.rs`.
//!
//! The UI tells the loader each frame what it wants, in priority order; anything
//! scrolled past is dropped from the queue instead of piling up.

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use eframe::egui::{self, ColorImage};
use image::{imageops, ImageBuffer, Pixel, RgbImage, RgbaImage};

use crate::raw;

/// Long edge of grid thumbnails, in pixels.
pub const THUMB_PX: u32 = 400;
/// Bytes read from the start of a JPG to find the camera's embedded thumbnail.
const PREVIEW_READ_BYTES: u64 = 128 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Preview,
    Thumb,
    Full,
    Original,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub shot: usize,
    pub kind: Kind,
}

pub struct Request {
    pub key: Key,
    pub path: PathBuf,
    /// Largest size wanted, `[width, height]` in pixels.
    pub max: [u32; 2],
}

pub struct Loaded {
    pub key: Key,
    pub generation: u64,
    /// The decoded image and the photo's full size `[width, height]` (as displayed).
    pub image: Result<(ColorImage, [u32; 2]), String>,
    /// For an animated GIF shown in the viewer: every frame and how long it shows
    /// (seconds). Empty for still images.
    pub animation: Vec<(ColorImage, f64)>,
}

/// Image types decoded by the `image` crate (PNG, GIF, WebP, TIFF, BMP).
const IMAGE_CRATE_EXTENSIONS: &[&str] = &["png", "gif", "webp", "tif", "tiff", "bmp"];
/// Image types decoded by Windows (HEIC/HEIF need the HEIF Image Extension, AVIF the
/// AV1 Video Extension).
const WINDOWS_DECODED_EXTENSIONS: &[&str] = &["heic", "heif", "avif"];
/// Most memory an animated GIF's frames may take in the viewer; frames beyond it are
/// dropped and the animation loops over the rest.
const ANIMATION_BYTES: usize = 256 * 1024 * 1024;

#[derive(Default)]
struct Queues {
    full: Vec<Request>,
    thumbs: Vec<Request>,
    in_flight: HashSet<Key>,
    generation: u64,
    shutdown: bool,
}

pub struct Loader {
    shared: Arc<(Mutex<Queues>, Condvar)>,
    results: Receiver<Loaded>,
}

impl Loader {
    pub fn new(ctx: egui::Context) -> Self {
        let shared = Arc::new((Mutex::new(Queues::default()), Condvar::new()));
        let (tx, rx) = channel();
        let workers = std::thread::available_parallelism()
            .map(|n| n.get().saturating_sub(1))
            .unwrap_or(3)
            .clamp(2, 8);
        for i in 0..workers {
            let shared = shared.clone();
            let tx: Sender<Loaded> = tx.clone();
            let ctx = ctx.clone();
            std::thread::Builder::new()
                .name(format!("decode-{i}"))
                .spawn(move || worker(shared, tx, ctx))
                .expect("failed to start decode thread");
        }
        Self { shared, results: rx }
    }

    /// Forget everything queued; results still in progress come back with an old
    /// generation and are ignored by the caller.
    pub fn reset(&self) -> u64 {
        let (lock, _) = &*self.shared;
        let mut q = lock.lock().unwrap();
        q.generation += 1;
        q.full.clear();
        q.thumbs.clear();
        q.in_flight.clear();
        q.generation
    }

    /// Replace the queue of full-size images (highest priority first).
    pub fn want_full(&self, requests: Vec<Request>) {
        self.replace(requests, |q| &mut q.full);
    }

    /// Replace the queue of grid thumbnails (highest priority first).
    pub fn want_thumbs(&self, requests: Vec<Request>) {
        self.replace(requests, |q| &mut q.thumbs);
    }

    fn replace(&self, mut requests: Vec<Request>, queue: impl Fn(&mut Queues) -> &mut Vec<Request>) {
        let (lock, cvar) = &*self.shared;
        let mut q = lock.lock().unwrap();
        requests.retain(|r| !q.in_flight.contains(&r.key));
        let has_work = !requests.is_empty();
        *queue(&mut q) = requests;
        drop(q);
        if has_work {
            cvar.notify_all();
        }
    }

    /// The caller has stored (or dropped) this result; it may be requested again.
    pub fn finished(&self, key: Key, generation: u64) {
        let (lock, _) = &*self.shared;
        let mut q = lock.lock().unwrap();
        if q.generation == generation {
            q.in_flight.remove(&key);
        }
    }

    pub fn try_recv(&self) -> Option<Loaded> {
        self.results.try_recv().ok()
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.shared;
        lock.lock().unwrap().shutdown = true;
        cvar.notify_all();
    }
}

fn worker(shared: Arc<(Mutex<Queues>, Condvar)>, tx: Sender<Loaded>, ctx: egui::Context) {
    let (lock, cvar) = &*shared;
    loop {
        let (request, generation) = {
            let mut q = lock.lock().unwrap();
            loop {
                if q.shutdown {
                    return;
                }
                let next = if !q.full.is_empty() {
                    Some(q.full.remove(0))
                } else if !q.thumbs.is_empty() {
                    Some(q.thumbs.remove(0))
                } else {
                    None
                };
                if let Some(r) = next {
                    q.in_flight.insert(r.key);
                    break (r, q.generation);
                }
                q = cvar.wait(q).unwrap();
            }
        };
        let (image, animation) = decode(&request.path, request.key.kind, request.max);
        if tx.send(Loaded { key: request.key, generation, image, animation }).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

/// Decodes one file at the size `kind` asks for, picking the decoder by file type. A
/// decoder that panics on a damaged file counts as a failed load, so it can't take the
/// worker thread down with it.
fn decode(path: &Path, kind: Kind, max: [u32; 2]) -> (Result<(ColorImage, [u32; 2]), String>, Vec<(ColorImage, f64)>) {
    std::panic::catch_unwind(|| {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
        let mut animation = Vec::new();
        let image = if raw::is_raw_extension(&ext) {
            decode_raw(path, kind, max)
        } else if IMAGE_CRATE_EXTENSIONS.contains(&ext.as_str()) || WINDOWS_DECODED_EXTENSIONS.contains(&ext.as_str()) {
            match kind {
                // No embedded thumbnail to grab quickly; the Thumb request decodes it.
                Kind::Preview => Err("no embedded thumbnail".to_string()),
                Kind::Full if ext == "gif" => decode_gif_animation(path, max).map(|(first, frames)| {
                    animation = frames;
                    first
                }),
                _ if WINDOWS_DECODED_EXTENSIONS.contains(&ext.as_str()) => decode_with_windows(path, max),
                _ => decode_with_image_crate(path, max),
            }
        } else if kind == Kind::Preview {
            decode_embedded_thumbnail(path)
        } else {
            decode_file(path, max)
        };
        (image, animation)
    })
    .unwrap_or_else(|_| (Err("the file looks damaged".to_string()), Vec::new()))
}

/// A small picture of a photo, made quickly for the drag-out stack: the camera's
/// embedded thumbnail where there is one (a few KB read), with black bars trimmed.
pub fn quick_picture(path: &Path, max: [u32; 2]) -> Option<ColorImage> {
    // A RAW's smallest preview is too tiny for this; its Thumb is a quick read anyway.
    let is_raw = path.extension().and_then(|e| e.to_str()).is_some_and(raw::is_raw_extension);
    let first = if is_raw { Kind::Thumb } else { Kind::Preview };
    let image = decode(path, first, max).0.or_else(|_| decode(path, Kind::Thumb, max).0);
    image.ok().map(|(img, _)| trim_black_bars(img))
}

/// Removes the black bars cameras put above and below (or beside) a thumbnail stored in
/// a different shape from the photo.
fn trim_black_bars(img: ColorImage) -> ColorImage {
    let [w, h] = img.size;
    let dark = |c: egui::Color32| c.r().max(c.g()).max(c.b()) < 12;
    let row_dark = |y: usize| (0..w).all(|x| dark(img.pixels[y * w + x]));
    let col_dark = |x: usize| (0..h).all(|y| dark(img.pixels[y * w + x]));
    let (mut top, mut bottom, mut left, mut right) = (0, h, 0, w);
    while top < bottom && row_dark(top) {
        top += 1;
    }
    while bottom > top && row_dark(bottom - 1) {
        bottom -= 1;
    }
    while left < right && col_dark(left) {
        left += 1;
    }
    while right > left && col_dark(right - 1) {
        right -= 1;
    }
    // An all-dark photo, or barely anything trimmed: keep it whole.
    if bottom - top < h / 2 || right - left < w / 2 || (top, bottom, left, right) == (0, h, 0, w) {
        return img;
    }
    let mut pixels = Vec::with_capacity((bottom - top) * (right - left));
    for y in top..bottom {
        pixels.extend_from_slice(&img.pixels[y * w + left..y * w + right]);
    }
    ColorImage { size: [right - left, bottom - top], pixels, ..img }
}

/// Shrinks `img` to fit within `max` (never enlarges).
fn fit_within<P>(img: ImageBuffer<P, Vec<P::Subpixel>>, max: [u32; 2]) -> ImageBuffer<P, Vec<P::Subpixel>>
where
    P: Pixel + 'static,
    P::Subpixel: 'static,
{
    let (w, h) = img.dimensions();
    if w <= max[0] && h <= max[1] {
        return img;
    }
    let scale = (max[0] as f32 / w as f32).min(max[1] as f32 / h as f32);
    let nw = ((w as f32 * scale).round() as u32).max(1);
    let nh = ((h as f32 * scale).round() as u32).max(1);
    imageops::resize(&img, nw, nh, imageops::FilterType::Triangle)
}

fn to_color_image(img: &RgbaImage) -> ColorImage {
    ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw())
}

/// PNG, GIF (first frame), WebP, TIFF, BMP, turned upright per their orientation tag.
fn decode_with_image_crate(path: &Path, max: [u32; 2]) -> Result<(ColorImage, [u32; 2]), String> {
    use image::{DynamicImage, ImageDecoder, ImageReader};
    let reader = ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .map_err(|e| e.to_string())?;
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().map_err(|e| e.to_string())?;
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    img.apply_orientation(orientation);
    let original = [img.width(), img.height()];
    let rgba = fit_within(img.into_rgba8(), max);
    Ok((to_color_image(&rgba), original))
}

/// HEIC / HEIF / AVIF through Windows' own decoders.
fn decode_with_windows(path: &Path, max: [u32; 2]) -> Result<(ColorImage, [u32; 2]), String> {
    let d = crate::wic::decode(path, max)?;
    let img = RgbaImage::from_raw(d.size[0], d.size[1], d.rgba).ok_or("pixel buffer size mismatch")?;
    let img = orient(img, d.orientation);
    Ok((to_color_image(&img), d.original))
}

/// Every frame of a GIF, sized to fit `max`, with how long each shows. Returns the
/// first frame on its own too (for the still view and zoom), with the GIF's size.
fn decode_gif_animation(path: &Path, max: [u32; 2]) -> Result<((ColorImage, [u32; 2]), Vec<(ColorImage, f64)>), String> {
    use image::codecs::gif::GifDecoder;
    use image::AnimationDecoder;
    let file = std::io::BufReader::new(std::fs::File::open(path).map_err(|e| e.to_string())?);
    let decoder = GifDecoder::new(file).map_err(|e| e.to_string())?;
    let mut frames = Vec::new();
    let mut original = [0, 0];
    let mut bytes = 0usize;
    for frame in decoder.into_frames() {
        let frame = frame.map_err(|e| e.to_string())?;
        let (num, den) = frame.delay().numer_denom_ms();
        // Browsers treat very short delays (0–10 ms) as 100 ms; so does Cullery.
        let ms = if den == 0 { 100.0 } else { num as f64 / den as f64 };
        let delay = if ms <= 10.0 { 0.1 } else { ms / 1000.0 };
        let buffer = frame.into_buffer();
        original = [buffer.width(), buffer.height()];
        let img = to_color_image(&fit_within(buffer, max));
        bytes += img.pixels.len() * 4;
        frames.push((img, delay));
        if bytes > ANIMATION_BYTES {
            break;
        }
    }
    let first = frames.first().ok_or("GIF has no frames")?.0.clone();
    if frames.len() == 1 {
        frames.clear(); // a still GIF
    }
    Ok(((first, original), frames))
}

/// Applies an EXIF orientation (1–8) so the image is upright.
fn orient<P>(img: ImageBuffer<P, Vec<P::Subpixel>>, orientation: u32) -> ImageBuffer<P, Vec<P::Subpixel>>
where
    P: Pixel + 'static,
{
    match orientation {
        2 => imageops::flip_horizontal(&img),
        3 => imageops::rotate180(&img),
        4 => imageops::flip_vertical(&img),
        5 => imageops::flip_horizontal(&imageops::rotate90(&img)),
        6 => imageops::rotate90(&img),
        7 => imageops::flip_horizontal(&imageops::rotate270(&img)),
        8 => imageops::rotate270(&img),
        _ => img,
    }
}

fn decode_file(path: &Path, max: [u32; 2]) -> Result<(ColorImage, [u32; 2]), String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let orientation = exif_orientation(&bytes).unwrap_or(1);
    decode_jpeg(&bytes, max, orientation)
}

/// Where each RAW file's previews are, so the file is only searched once. Remembered
/// with the file's size and time: another card can have a file at the very same path.
fn raw_previews(path: &Path) -> Result<Arc<raw::Previews>, String> {
    type Stamp = (u64, Option<std::time::SystemTime>);
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, (Stamp, Arc<raw::Previews>)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let stamp = (meta.len(), meta.modified().ok());
    if let Some((_, found)) = cache.lock().unwrap().get(path).filter(|(s, _)| *s == stamp) {
        return Ok(found.clone());
    }
    let found = Arc::new(raw::find_previews(path).map_err(|e| e.to_string())?);
    let mut cache = cache.lock().unwrap();
    if cache.len() > 20_000 {
        cache.clear();
    }
    cache.insert(path.to_path_buf(), (stamp, found.clone()));
    Ok(found)
}

/// Shows a RAW file using the JPEG the camera embedded in it: the smallest one
/// that's big enough for what's being drawn.
fn decode_raw(path: &Path, kind: Kind, max: [u32; 2]) -> Result<(ColorImage, [u32; 2]), String> {
    let previews = raw_previews(path)?;
    let choice = match kind {
        Kind::Preview => previews.smallest(),
        Kind::Thumb => previews.at_least([THUMB_PX, THUMB_PX]),
        Kind::Full => previews.at_least(max),
        Kind::Original => previews.largest(),
    }
    .ok_or("no embedded preview")?;
    let bytes = raw::read_preview(path, choice).map_err(|e| e.to_string())?;
    // The JPEG's own orientation if it has one, else the RAW's.
    let orientation = exif_orientation(&bytes).or(previews.orientation).unwrap_or(1);
    let (image, _) = decode_jpeg(&bytes, max, orientation)?;
    // Report the photo's size as the biggest preview, so zoom percentages are right.
    let biggest = previews.largest().unwrap_or(choice);
    let original = if (5..=8).contains(&orientation) {
        [biggest.height, biggest.width]
    } else {
        [biggest.width, biggest.height]
    };
    Ok((image, original))
}

/// The small JPEG thumbnail cameras store in the EXIF block at the start of the file.
fn decode_embedded_thumbnail(path: &Path) -> Result<(ColorImage, [u32; 2]), String> {
    let mut head = Vec::with_capacity(PREVIEW_READ_BYTES as usize);
    std::fs::File::open(path)
        .and_then(|f| f.take(PREVIEW_READ_BYTES).read_to_end(&mut head))
        .map_err(|e| e.to_string())?;
    let exif = exif::Reader::new()
        .read_from_container(&mut Cursor::new(&head))
        .map_err(|e| e.to_string())?;
    let field_u32 = |tag| {
        exif.get_field(tag, exif::In::THUMBNAIL)
            .and_then(|f| f.value.get_uint(0))
            .map(|v| v as usize)
    };
    let (Some(offset), Some(len)) = (
        field_u32(exif::Tag::JPEGInterchangeFormat),
        field_u32(exif::Tag::JPEGInterchangeFormatLength),
    ) else {
        return Err("no embedded thumbnail".into());
    };
    let thumb = exif
        .buf()
        .get(offset..offset + len)
        .ok_or("embedded thumbnail out of range")?;
    let orientation = exif
        .get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
        .unwrap_or(1);
    decode_jpeg(thumb, [THUMB_PX, THUMB_PX], orientation)
}

fn exif_orientation(jpeg: &[u8]) -> Option<u32> {
    exif::Reader::new()
        .read_from_container(&mut Cursor::new(jpeg))
        .ok()
        .and_then(|e| {
            e.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
                .and_then(|f| f.value.get_uint(0))
        })
}

/// Decodes a JPEG no larger than `max` (after rotation), using the decoder's
/// built-in downscaling so big files decode several times faster.
fn decode_jpeg(bytes: &[u8], max: [u32; 2], orientation: u32) -> Result<(ColorImage, [u32; 2]), String> {
    // Orientations 5–8 are stored sideways: the stored width becomes the shown height.
    let sideways = (5..=8).contains(&orientation);
    let [max_w, max_h] = if sideways { [max[1], max[0]] } else { max };

    let mut decoder = jpeg_decoder::Decoder::new(bytes);
    decoder.read_info().map_err(|e| e.to_string())?;
    let info = decoder.info().ok_or("missing image info")?;
    let (ow, oh) = (info.width as u32, info.height as u32);
    let original = if sideways { [oh, ow] } else { [ow, oh] };
    let (w, h) = decoder
        .scale(max_w.min(u16::MAX as u32) as u16, max_h.min(u16::MAX as u32) as u16)
        .map_err(|e| e.to_string())?;
    let pixels = decoder.decode().map_err(|e| e.to_string())?;
    let format = decoder.info().ok_or("missing image info")?.pixel_format;
    let (w, h) = (w as u32, h as u32);

    use jpeg_decoder::PixelFormat;
    let rgb: Vec<u8> = match format {
        PixelFormat::RGB24 => pixels,
        PixelFormat::L8 => pixels.iter().flat_map(|&l| [l, l, l]).collect(),
        PixelFormat::L16 => pixels
            .chunks_exact(2)
            .flat_map(|p| [(u16::from_ne_bytes([p[0], p[1]]) >> 8) as u8; 3])
            .collect(),
        PixelFormat::CMYK32 => pixels
            .chunks_exact(4)
            .flat_map(|p| {
                let k = 255 - p[3] as u32;
                [p[0], p[1], p[2]].map(|c| ((255 - c as u32) * k / 255) as u8)
            })
            .collect(),
    };
    let img = RgbImage::from_raw(w, h, rgb).ok_or("pixel buffer size mismatch")?;

    // The decoder only scales by 1/2, 1/4 or 1/8; finish the job.
    let img = orient(fit_within(img, [max_w, max_h]), orientation);
    let size = [img.width() as usize, img.height() as usize];
    Ok((ColorImage::from_rgb(size, img.as_raw()), original))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("cullery-fmt-{}-{name}", std::process::id()))
    }

    /// A 600×400 gradient, so scaled results are easy to check.
    fn test_image() -> RgbaImage {
        RgbaImage::from_fn(600, 400, |x, y| image::Rgba([(x / 3) as u8, (y / 2) as u8, 90, 255]))
    }

    /// PNG, WebP, TIFF and BMP decode, scaled to fit, reporting the full size.
    #[test]
    fn decodes_png_webp_tiff_bmp() {
        for ext in ["png", "webp", "tiff", "bmp"] {
            let path = temp(&format!("still.{ext}"));
            test_image().save(&path).unwrap_or_else(|e| panic!("encoding {ext}: {e}"));
            let decoded = decode_with_image_crate(&path, [300, 300]);
            std::fs::remove_file(&path).unwrap();
            let (img, original) = decoded.unwrap_or_else(|e| panic!("decoding {ext}: {e}"));
            assert_eq!(original, [600, 400], "{ext}");
            assert_eq!(img.size, [300, 200], "{ext}");
        }
    }

    /// An animated GIF keeps every frame and its timing; a 0 ms delay plays as 100 ms.
    #[test]
    fn decodes_animated_gif_frames() {
        use image::codecs::gif::{GifEncoder, Repeat};
        use image::{Delay, Frame};
        let path = temp("anim.gif");
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut encoder = GifEncoder::new(file);
            encoder.set_repeat(Repeat::Infinite).unwrap();
            for (i, ms) in [(0u8, 50u32), (1, 200), (2, 0)] {
                let frame = RgbaImage::from_pixel(120, 80, image::Rgba([i * 100, 0, 0, 255]));
                encoder.encode_frame(Frame::from_parts(frame, 0, 0, Delay::from_numer_denom_ms(ms, 1))).unwrap();
            }
        }
        let decoded = decode_gif_animation(&path, [60, 60]);
        // Its first frame alone, for the grid.
        let still = decode_with_image_crate(&path, [400, 400]);
        std::fs::remove_file(&path).unwrap();

        let ((first, original), frames) = decoded.unwrap();
        assert_eq!(original, [120, 80]);
        assert_eq!(first.size, [60, 40]);
        let delays: Vec<f64> = frames.iter().map(|(_, d)| (d * 1000.0).round()).collect();
        assert_eq!(delays, [50.0, 200.0, 100.0]);
        assert_eq!(still.unwrap().0.size, [120, 80]);
    }

    /// Decodes real RAW files' embedded previews at each size, e.g.
    /// `$env:CULLERY_RAW_SAMPLES='F:\DCIM\100CANON'; cargo test --release raw_decode -- --nocapture`
    #[test]
    fn raw_decode_samples() {
        let Some(dir) = std::env::var_os("CULLERY_RAW_SAMPLES") else { return };
        let files: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|e| e.to_str()).is_some_and(raw::is_raw_extension))
            .take(3)
            .collect();
        for path in files {
            for (kind, max) in [(Kind::Preview, [THUMB_PX; 2]), (Kind::Thumb, [THUMB_PX; 2]), (Kind::Full, [2560, 1440]), (Kind::Original, [16384; 2])] {
                let start = std::time::Instant::now();
                let (img, original) = decode_raw(&path, kind, max).unwrap();
                println!(
                    "{} {kind:?}: {:?} of {:?} in {:?}",
                    path.file_name().unwrap().to_string_lossy(),
                    img.size,
                    original,
                    start.elapsed()
                );
            }
        }
    }

    /// Runs against the sample shots from `tools/make-test-photos.ps1`, if present.
    #[test]
    fn decodes_sample_shots_at_reduced_size() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("test-photos");
        let Ok(entries) = std::fs::read_dir(&dir) else { return };
        let jpgs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("jpg")))
            .take(5)
            .collect();
        for path in jpgs {
            let start = std::time::Instant::now();
            let (thumb, original) = decode_file(&path, [THUMB_PX, THUMB_PX]).unwrap();
            assert!(original[0].max(original[1]) == 6000);
            let thumb_time = start.elapsed();
            assert!(thumb.size[0] <= THUMB_PX as usize && thumb.size[1] <= THUMB_PX as usize);
            assert_eq!(thumb.size[0].max(thumb.size[1]), THUMB_PX as usize);

            let start = std::time::Instant::now();
            let (full, _) = decode_file(&path, [2560, 1440]).unwrap();
            println!(
                "{}: thumb {:?} in {:?}, full {:?} in {:?}",
                path.file_name().unwrap().to_string_lossy(),
                thumb.size,
                thumb_time,
                full.size,
                start.elapsed()
            );
            assert!(full.size[0] <= 2560 && full.size[1] <= 1440);
        }
    }
}
