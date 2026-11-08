//! Decodes JPGs on background threads so scrolling never waits on the disk.
//!
//! Three sizes per shot:
//! - `Preview`: the tiny thumbnail the camera stores inside the JPG. Only the first
//!   few KB of the file are read, so it shows almost instantly, even from an SD card.
//! - `Thumb`: a sharp grid thumbnail decoded from the full JPG at reduced scale.
//! - `Full`: the image sized to the viewer.
//! - `Original`: every pixel, for zooming in.
//!
//! The UI tells the loader each frame what it wants, in priority order; anything
//! scrolled past is dropped from the queue instead of piling up.

use std::collections::HashSet;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

use eframe::egui::{self, ColorImage};
use image::{imageops, RgbImage};

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
}

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
        let image = match request.key.kind {
            Kind::Preview => decode_embedded_thumbnail(&request.path),
            Kind::Thumb | Kind::Full | Kind::Original => decode_file(&request.path, request.max),
        };
        if tx.send(Loaded { key: request.key, generation, image }).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

fn decode_file(path: &Path, max: [u32; 2]) -> Result<(ColorImage, [u32; 2]), String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let orientation = exif_orientation(&bytes);
    decode_jpeg(&bytes, max, orientation)
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

fn exif_orientation(jpeg: &[u8]) -> u32 {
    exif::Reader::new()
        .read_from_container(&mut Cursor::new(jpeg))
        .ok()
        .and_then(|e| {
            e.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
                .and_then(|f| f.value.get_uint(0))
        })
        .unwrap_or(1)
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
        PixelFormat::L16 => pixels.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0]]).collect(),
        PixelFormat::CMYK32 => pixels
            .chunks_exact(4)
            .flat_map(|p| {
                let k = 255 - p[3] as u32;
                [p[0], p[1], p[2]].map(|c| ((255 - c as u32) * k / 255) as u8)
            })
            .collect(),
    };
    let mut img = RgbImage::from_raw(w, h, rgb).ok_or("pixel buffer size mismatch")?;

    // The decoder only scales by 1/2, 1/4 or 1/8; finish the job.
    if w > max_w || h > max_h {
        let scale = (max_w as f32 / w as f32).min(max_h as f32 / h as f32);
        let nw = ((w as f32 * scale).round() as u32).max(1);
        let nh = ((h as f32 * scale).round() as u32).max(1);
        img = imageops::resize(&img, nw, nh, imageops::FilterType::Triangle);
    }

    let img = match orientation {
        2 => imageops::flip_horizontal(&img),
        3 => imageops::rotate180(&img),
        4 => imageops::flip_vertical(&img),
        5 => imageops::flip_horizontal(&imageops::rotate90(&img)),
        6 => imageops::rotate90(&img),
        7 => imageops::flip_horizontal(&imageops::rotate270(&img)),
        8 => imageops::rotate270(&img),
        _ => img,
    };
    let size = [img.width() as usize, img.height() as usize];
    Ok((ColorImage::from_rgb(size, img.as_raw()), original))
}

#[cfg(test)]
mod tests {
    use super::*;

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
