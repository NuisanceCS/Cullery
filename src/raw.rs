//! Finds the JPEG previews cameras embed inside RAW files.
//!
//! Decoding real RAW sensor data is slow and looks flat without the camera's
//! processing, so Cullery shows the camera's own JPEGs instead — the same ones the
//! camera's screen uses. Only the bytes of the chosen preview are read.
//!
//! Supported containers:
//! - **CR3** (Canon): ISO media boxes. A 160×120 thumbnail (`THMB`), a ~1620×1080
//!   preview (`PRVW`), and a full-size JPEG as the first track.
//! - **TIFF-based** (CR2, NEF, ARW, DNG, ORF, RW2, PEF, SRW…): JPEGs referenced from
//!   the TIFF directories, including sub-directories and Panasonic's `JpgFromRaw`.
//! - **RAF** (Fujifilm): one JPEG at a fixed place in the header.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

/// File extensions treated as RAW.
pub const RAW_EXTENSIONS: &[&str] = &[
    "cr3", "cr2", "crw", "nef", "nrw", "arw", "srf", "sr2", "dng", "raf", "orf", "rw2", "pef", "srw",
];

pub fn is_raw_extension(ext: &str) -> bool {
    RAW_EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(ext))
}

/// One embedded JPEG: where it is and how big it is (as stored, before rotation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preview {
    pub offset: u64,
    pub len: u64,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Previews {
    pub jpegs: Vec<Preview>,
    /// EXIF orientation from the RAW's own metadata, if it has one.
    pub orientation: Option<u32>,
}

impl Previews {
    pub fn largest(&self) -> Option<&Preview> {
        self.jpegs.iter().max_by_key(|p| p.width as u64 * p.height as u64)
    }

    pub fn smallest(&self) -> Option<&Preview> {
        self.jpegs.iter().min_by_key(|p| p.width as u64 * p.height as u64)
    }

    /// The smallest preview big enough to fill a `max` box without enlarging,
    /// or the largest there is.
    pub fn at_least(&self, max: [u32; 2]) -> Option<&Preview> {
        let (want_long, want_short) = (max[0].max(max[1]), max[0].min(max[1]));
        self.jpegs
            .iter()
            .filter(|p| p.width.max(p.height) >= want_long || p.width.min(p.height) >= want_short)
            .min_by_key(|p| p.width as u64 * p.height as u64)
            .or_else(|| self.largest())
    }
}

/// Lists the usable JPEG previews in a RAW file.
pub fn find_previews(path: &Path) -> io::Result<Previews> {
    let mut src = Source::open(path)?;
    let head = src.read_at(0, 16)?;
    let mut found = Previews::default();
    if head.len() >= 8 && &head[4..8] == b"ftyp" {
        cr3(&mut src, &mut found)?;
    } else if head.starts_with(b"FUJIFILM") {
        raf(&mut src, &mut found)?;
    } else if head.starts_with(b"II") || head.starts_with(b"MM") {
        tiff(&mut |off, n| src.read_at(off, n), 0, &mut found)?;
    } else {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "unrecognized RAW format"));
    }

    // Keep only real, displayable JPEGs, and learn their sizes from their headers.
    let mut seen = HashSet::new();
    let candidates = std::mem::take(&mut found.jpegs);
    for c in candidates {
        if c.len < 4 || !seen.insert(c.offset) {
            continue;
        }
        let Ok(head) = src.read_at(c.offset, c.len.min(256 * 1024) as usize) else { continue };
        if let Some((width, height)) = jpeg_size(&head) {
            found.jpegs.push(Preview { width, height, ..c });
        }
    }
    if found.jpegs.is_empty() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "no embedded preview"));
    }
    Ok(found)
}

pub fn read_preview(path: &Path, preview: &Preview) -> io::Result<Vec<u8>> {
    Source::open(path)?.read_at(preview.offset, preview.len as usize)
}

// ------------------------------------------------------------------ file access

struct Source {
    file: File,
    len: u64,
}

impl Source {
    fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self { file, len })
    }

    /// Up to `n` bytes at `offset` (fewer at the end of the file).
    fn read_at(&mut self, offset: u64, n: usize) -> io::Result<Vec<u8>> {
        if offset >= self.len {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "offset past end of file"));
        }
        let n = n.min((self.len - offset) as usize);
        let mut buf = vec![0; n];
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(&mut buf)?;
        Ok(buf)
    }
}

fn candidate(offset: u64, len: u64) -> Preview {
    Preview { offset, len, width: 0, height: 0 }
}

fn be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn be64(b: &[u8]) -> u64 {
    u64::from_be_bytes(b[..8].try_into().unwrap())
}

/// Width and height from a JPEG's frame header, if it's a JPEG we can show
/// (baseline, extended or progressive; grayscale or color). Lossless JPEG — what
/// some RAWs use for the sensor data itself — is rejected.
pub fn jpeg_size(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() < 4 || b[0] != 0xFF || b[1] != 0xD8 {
        return None;
    }
    let mut i = 2;
    while i + 4 <= b.len() {
        if b[i] != 0xFF {
            return None;
        }
        let marker = b[i + 1];
        if marker == 0xFF {
            i += 1; // fill byte
            continue;
        }
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        let len = be16(&b[i + 2..]) as usize;
        match marker {
            0xC0..=0xC2 => {
                let f = b.get(i + 4..i + 10)?;
                let (height, width, components) = (be16(&f[1..]) as u32, be16(&f[3..]) as u32, f[5]);
                let ok = width > 0 && height > 0 && (components == 1 || components == 3);
                return ok.then_some((width, height));
            }
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF | 0xDA => return None,
            _ => i += 2 + len,
        }
    }
    None
}

// ------------------------------------------------------------------ CR3

const CANON_UUID: [u8; 16] = [
    0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x6a, 0x48,
];
const PREVIEW_UUID: [u8; 16] = [
    0xea, 0xf4, 0x2b, 0x5e, 0x1c, 0x98, 0x4b, 0x88, 0xb9, 0xfb, 0xb7, 0xdc, 0x40, 0x6e, 0x4d, 0x16,
];

/// An ISO media box header: (total size, header size). A size of 0 means "to the end".
fn box_header(h: &[u8], remaining: u64) -> Option<(u64, u64)> {
    if h.len() < 8 {
        return None;
    }
    let (size, header) = match be32(h) as u64 {
        1 if h.len() >= 16 => (be64(&h[8..]), 16),
        0 => (remaining, 8),
        s => (s, 8),
    };
    (size >= header && size <= remaining).then_some((size, header))
}

/// Child boxes of an in-memory box payload: (type, payload start, payload end).
fn children(data: &[u8]) -> Vec<([u8; 4], usize, usize)> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos + 8 <= data.len() {
        let Some((size, header)) = box_header(&data[pos..], (data.len() - pos) as u64) else { break };
        let kind: [u8; 4] = data[pos + 4..pos + 8].try_into().unwrap();
        out.push((kind, pos + header as usize, pos + size as usize));
        pos += size as usize;
    }
    out
}

/// The JPEG inside a Canon THMB / PRVW payload: it follows a small header.
fn inner_jpeg(payload: &[u8], payload_offset: u64) -> Option<Preview> {
    let start = payload.windows(2).take(48).position(|w| w == [0xFF, 0xD8])?;
    // The JPEG's length is the 32-bit number just before it.
    let len = if start >= 4 { be32(&payload[start - 4..]) as u64 } else { 0 };
    let avail = (payload.len() - start) as u64;
    let len = if len == 0 || len > avail { avail } else { len };
    Some(candidate(payload_offset + start as u64, len))
}

fn cr3(src: &mut Source, out: &mut Previews) -> io::Result<()> {
    let mut pos = 0;
    while pos + 8 <= src.len {
        let h = src.read_at(pos, 16)?;
        let Some((size, header)) = box_header(&h, src.len - pos) else { break };
        match &h[4..8] {
            b"moov" => {
                let data = src.read_at(pos + header, (size - header) as usize)?;
                cr3_moov(&data, pos + header, out);
            }
            b"uuid" if size >= header + 16 => {
                let uuid = src.read_at(pos + header, 16)?;
                if uuid == PREVIEW_UUID {
                    // A small header, then a PRVW box holding the JPEG.
                    let start = pos + header + 16;
                    let data = src.read_at(start, (size - header - 16).min(128) as usize)?;
                    if let Some(at) = data.windows(4).position(|w| w == b"PRVW") {
                        let box_start = at.saturating_sub(4);
                        let box_size = be32(&data[box_start..]) as u64;
                        let payload_start = start + at as u64 + 4;
                        let payload_len = (box_size.saturating_sub(8)).min(src.len - payload_start);
                        let payload = src.read_at(payload_start, payload_len.min(128) as usize)?;
                        if let Some(mut p) = inner_jpeg(&payload, payload_start) {
                            p.len = p.len.max(payload_len - (p.offset - payload_start));
                            out.jpegs.push(p);
                        }
                    }
                }
            }
            _ => {}
        }
        pos += size;
    }
    Ok(())
}

fn cr3_moov(moov: &[u8], moov_offset: u64, out: &mut Previews) {
    let mut first_track = true;
    for (kind, start, end) in children(moov) {
        let payload = &moov[start..end];
        match &kind {
            b"uuid" if payload.len() >= 16 && payload[..16] == CANON_UUID => {
                for (k, s, e) in children(&payload[16..]) {
                    let inner = &payload[16 + s..16 + e];
                    let offset = moov_offset + (start + 16 + s) as u64;
                    match &k {
                        b"THMB" => {
                            if let Some(p) = inner_jpeg(inner, offset) {
                                out.jpegs.push(p);
                            }
                        }
                        // CMT1 is a TIFF holding the main EXIF block, including orientation.
                        b"CMT1" => {
                            let mut found = Previews::default();
                            let mut read = |off: u64, n: usize| -> io::Result<Vec<u8>> {
                                let off = off as usize;
                                inner
                                    .get(off..(off + n).min(inner.len()))
                                    .map(<[u8]>::to_vec)
                                    .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))
                            };
                            let _ = tiff(&mut read, 0, &mut found);
                            out.orientation = out.orientation.or(found.orientation);
                        }
                        _ => {}
                    }
                }
            }
            // The first track is the full-size JPEG.
            b"trak" if first_track => {
                first_track = false;
                if let Some(p) = track_sample(payload) {
                    out.jpegs.push(p);
                }
            }
            _ => {}
        }
    }
}

/// Offset and size of a track's first sample (moov/trak/mdia/minf/stbl).
fn track_sample(trak: &[u8]) -> Option<Preview> {
    let find = |data: &[u8], name: &[u8; 4]| children(data).into_iter().find(|(k, _, _)| k == name).map(|(_, s, e)| data[s..e].to_vec());
    let mdia = find(trak, b"mdia")?;
    let minf = find(&mdia, b"minf")?;
    let stbl = find(&minf, b"stbl")?;
    let stsz = find(&stbl, b"stsz")?;
    let size = match be32(stsz.get(4..8)?) {
        0 => be32(stsz.get(12..16)?),
        s => s,
    } as u64;
    let offset = if let Some(co64) = find(&stbl, b"co64") {
        be64(co64.get(8..16)?)
    } else {
        be32(find(&stbl, b"stco")?.get(8..12)?) as u64
    };
    Some(candidate(offset, size))
}

// ------------------------------------------------------------------ RAF

fn raf(src: &mut Source, out: &mut Previews) -> io::Result<()> {
    let h = src.read_at(84, 8)?;
    if h.len() == 8 {
        out.jpegs.push(candidate(be32(&h) as u64, be32(&h[4..]) as u64));
    }
    Ok(())
}

// ------------------------------------------------------------------ TIFF

const TAG_COMPRESSION: u16 = 0x0103;
const TAG_STRIP_OFFSETS: u16 = 0x0111;
const TAG_ORIENTATION: u16 = 0x0112;
const TAG_STRIP_BYTES: u16 = 0x0117;
const TAG_SUB_IFDS: u16 = 0x014A;
const TAG_JPEG_OFFSET: u16 = 0x0201;
const TAG_JPEG_LENGTH: u16 = 0x0202;
/// Panasonic RW2: a complete JPEG stored as one value.
const TAG_JPG_FROM_RAW: u16 = 0x002E;

struct Entry {
    tag: u16,
    kind: u16,
    count: u32,
    value: [u8; 4],
}

/// Walks a TIFF structure at `base`, collecting JPEG previews and the orientation.
/// The magic number isn't checked: ORF and RW2 use their own.
fn tiff(read: &mut dyn FnMut(u64, usize) -> io::Result<Vec<u8>>, base: u64, out: &mut Previews) -> io::Result<()> {
    let h = read(base, 8)?;
    let le = match h.get(..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "not a TIFF")),
    };
    let u16_at = |b: &[u8]| if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) };
    let u32_at = |b: &[u8]| if le { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { be32(b) };

    let first = u32_at(&h[4..]) as u64;
    let mut queue = vec![first];
    let mut seen = HashSet::new();
    while let Some(ifd) = queue.pop() {
        if ifd == 0 || !seen.insert(ifd) || seen.len() > 64 {
            continue;
        }
        let Ok(count) = read(base + ifd, 2) else { continue };
        if count.len() < 2 {
            continue;
        }
        let n = u16_at(&count) as usize;
        if n == 0 || n > 1000 {
            continue;
        }
        let Ok(raw) = read(base + ifd + 2, n * 12 + 4) else { continue };
        if raw.len() < n * 12 {
            continue;
        }
        let entries: Vec<Entry> = raw
            .chunks_exact(12)
            .take(n)
            .map(|e| Entry { tag: u16_at(e), kind: u16_at(&e[2..]), count: u32_at(&e[4..]), value: e[8..12].try_into().unwrap() })
            .collect();
        if raw.len() >= n * 12 + 4 {
            queue.push(u32_at(&raw[n * 12..]) as u64);
        }

        // Numbers in an entry: inline if they fit in 4 bytes, otherwise stored elsewhere.
        let mut values = |e: &Entry| -> Vec<u64> {
            let size = match e.kind {
                3 => 2,
                4 | 13 => 4,
                _ => return Vec::new(),
            };
            let count = (e.count as usize).min(64);
            let bytes = if count * size <= 4 {
                e.value.to_vec()
            } else {
                read(base + u32_at(&e.value) as u64, count * size).unwrap_or_default()
            };
            bytes
                .chunks_exact(size)
                .take(count)
                .map(|b| if size == 2 { u16_at(b) as u64 } else { u32_at(b) as u64 })
                .collect()
        };
        let get = |tag| entries.iter().find(|e| e.tag == tag);
        let first_value = |values: Vec<u64>| values.first().copied();

        if ifd == first {
            if let Some(e) = get(TAG_ORIENTATION) {
                if let Some(o) = first_value(values(e)) {
                    out.orientation.get_or_insert(o as u32);
                }
            }
        }
        if let Some(e) = get(TAG_SUB_IFDS) {
            queue.extend(values(e));
        }
        if let (Some(o), Some(l)) = (get(TAG_JPEG_OFFSET), get(TAG_JPEG_LENGTH)) {
            if let (Some(o), Some(l)) = (first_value(values(o)), first_value(values(l))) {
                out.jpegs.push(candidate(base + o, l));
            }
        }
        // A single strip of JPEG data (CR2's full-size preview, DNG previews). Lossless
        // sensor data uses the same compression code; jpeg_size() filters it out later.
        let jpeg_compressed = get(TAG_COMPRESSION).and_then(|e| first_value(values(e))).is_some_and(|c| c == 6 || c == 7);
        if jpeg_compressed {
            if let (Some(o), Some(l)) = (get(TAG_STRIP_OFFSETS), get(TAG_STRIP_BYTES)) {
                if o.count == 1 && l.count == 1 {
                    if let (Some(o), Some(l)) = (first_value(values(o)), first_value(values(l))) {
                        out.jpegs.push(candidate(base + o, l));
                    }
                }
            }
        }
        if let Some(e) = get(TAG_JPG_FROM_RAW) {
            if e.count > 4 {
                out.jpegs.push(candidate(base + u32_at(&e.value) as u64, e.count as u64));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Just enough of a JPEG for jpeg_size(): start marker and a baseline frame header.
    fn fake_jpeg(w: u16, h: u16) -> Vec<u8> {
        let mut j = vec![0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08];
        j.extend(h.to_be_bytes());
        j.extend(w.to_be_bytes());
        j.extend([3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        j.extend([0xFF, 0xD9]);
        j
    }

    #[test]
    fn reads_jpeg_frame_size_and_rejects_lossless() {
        assert_eq!(jpeg_size(&fake_jpeg(6000, 4000)), Some((6000, 4000)));
        let mut lossless = fake_jpeg(10, 10);
        lossless[3] = 0xC3;
        assert_eq!(jpeg_size(&lossless), None);
        assert_eq!(jpeg_size(b"not a jpeg"), None);
    }

    #[test]
    fn finds_tiff_previews_and_orientation() {
        // Little-endian TIFF: IFD0 has orientation 6 and a JPEG via JPEGInterchangeFormat.
        let jpeg = fake_jpeg(1616, 1080);
        let mut t = b"II*\0".to_vec();
        t.extend(8u32.to_le_bytes());
        let entries: [(u16, u16, u32, u32); 3] = [
            (TAG_ORIENTATION, 3, 1, 6),
            (TAG_JPEG_OFFSET, 4, 1, 8 + 2 + 3 * 12 + 4),
            (TAG_JPEG_LENGTH, 4, 1, jpeg.len() as u32),
        ];
        t.extend(3u16.to_le_bytes());
        for (tag, kind, count, value) in entries {
            t.extend(tag.to_le_bytes());
            t.extend(kind.to_le_bytes());
            t.extend(count.to_le_bytes());
            t.extend(value.to_le_bytes());
        }
        t.extend(0u32.to_le_bytes());
        t.extend(&jpeg);

        let path = std::env::temp_dir().join(format!("cullery-raw-test-{}.nef", std::process::id()));
        std::fs::write(&path, &t).unwrap();
        let found = find_previews(&path);
        std::fs::remove_file(&path).unwrap();
        let found = found.unwrap();
        assert_eq!(found.orientation, Some(6));
        assert_eq!(found.jpegs.len(), 1);
        assert_eq!((found.jpegs[0].width, found.jpegs[0].height), (1616, 1080));
    }

    /// Probes real RAW files, e.g.
    /// `$env:CULLERY_RAW_SAMPLES='F:\DCIM\100CANON'; cargo test --release raw_samples -- --nocapture`
    #[test]
    fn raw_samples() {
        let Some(dir) = std::env::var_os("CULLERY_RAW_SAMPLES") else { return };
        let files: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|e| e.to_str()).is_some_and(is_raw_extension))
            .take(5)
            .collect();
        assert!(!files.is_empty(), "no RAW files found");
        for path in files {
            let start = std::time::Instant::now();
            let found = find_previews(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let sizes: Vec<String> = found.jpegs.iter().map(|p| format!("{}x{} ({} KB)", p.width, p.height, p.len / 1024)).collect();
            println!(
                "{}: {} | orientation {:?} | found in {:?}",
                path.file_name().unwrap().to_string_lossy(),
                sizes.join(", "),
                found.orientation,
                start.elapsed()
            );
            let big = found.largest().unwrap();
            let bytes = read_preview(&path, big).unwrap();
            assert_eq!(&bytes[..2], &[0xFF, 0xD8], "largest preview isn't a JPEG");
        }
    }
}
