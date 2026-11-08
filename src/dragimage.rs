//! The picture that follows the cursor when photos are dragged out: up to three of
//! them fanned into a little stack, with a badge saying how many there are. Windows
//! carries it under the cursor everywhere, over Lightroom and Explorer too.

use eframe::egui::{Color32, ColorImage};

/// A drag picture: premultiplied BGRA pixels, top row first, and where the cursor
/// holds it.
pub struct DragImage {
    pub size: [usize; 2],
    pub bgra: Vec<u8>,
    pub hotspot: [i32; 2],
}

/// Long edge of each photo card, at 100% display scaling.
const CARD: f32 = 104.0;
/// White frame around each photo.
const BORDER: f32 = 3.0;
const RADIUS: f32 = 4.0;
const SHADOW: f32 = 8.0;

/// One card's place in the stack: turn (degrees) and offset from the stack's center.
fn layout(cards: usize) -> &'static [(f32, [f32; 2])] {
    match cards {
        0 | 1 => &[(0.0, [0.0, 0.0])],
        2 => &[(0.0, [0.0, 0.0]), (-8.0, [-5.0, -3.0])],
        _ => &[(0.0, [0.0, 0.0]), (7.0, [7.0, -4.0]), (-9.0, [-7.0, -2.0])],
    }
}

/// The stack for `pictures` (front first; up to three are used) of `count` photos.
/// `scale` is the display's scaling (1.5 at 150%).
pub fn stack(pictures: &[ColorImage], count: usize, accent: Color32, scale: f32) -> DragImage {
    let s = scale.max(0.5);
    let side = (CARD * 1.75 * s).ceil() as usize;
    let mut canvas = vec![[0.0_f32; 4]; side * side];
    let center = [side as f32 / 2.0, side as f32 / 2.0 + 6.0 * s];

    let placeholder = ColorImage::new([3, 2], vec![Color32::from_gray(90); 6]);
    let pictures: Vec<&ColorImage> =
        if pictures.is_empty() { vec![&placeholder] } else { pictures.iter().take(3).collect() };
    let places = layout(pictures.len());
    let mut front_half = [0.0, 0.0];
    // Back to front, so the front card is drawn last, on top.
    for (i, pic) in pictures.iter().enumerate().rev() {
        let (turn, offset) = places[i];
        let [pw, ph] = [pic.size[0].max(1) as f32, pic.size[1].max(1) as f32];
        let fit = CARD * s / pw.max(ph);
        let inner = [pw * fit, ph * fit];
        let half = [inner[0] / 2.0 + BORDER * s, inner[1] / 2.0 + BORDER * s];
        if i == 0 {
            front_half = half;
        }
        let c = [center[0] + offset[0] * s, center[1] + offset[1] * s];
        draw_card(&mut canvas, side, pic, c, half, inner, turn.to_radians(), s);
    }

    if count > 1 {
        let text = count.to_string();
        let corner = [center[0] + front_half[0] - 3.0 * s, center[1] - front_half[1] + 3.0 * s];
        draw_badge(&mut canvas, side, &text, corner, accent, s);
    }

    let mut bgra = Vec::with_capacity(side * side * 4);
    for [r, g, b, a] in canvas {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        bgra.extend_from_slice(&[q(b), q(g), q(r), q(a)]);
    }
    let hotspot = [center[0].round() as i32, (center[1] + front_half[1] * 0.55).round() as i32];
    DragImage { size: [side, side], bgra, hotspot }
}

/// Signed distance from `p` (relative to the center) to a rounded rectangle.
fn rounded_rect(p: [f32; 2], half: [f32; 2], radius: f32) -> f32 {
    let q = [p[0].abs() - (half[0] - radius), p[1].abs() - (half[1] - radius)];
    let outside = (q[0].max(0.0).powi(2) + q[1].max(0.0).powi(2)).sqrt();
    outside + q[0].max(q[1]).min(0.0) - radius
}

/// Lays premultiplied `src` over `dst`.
fn over(dst: &mut [f32; 4], src: [f32; 4]) {
    let keep = 1.0 - src[3];
    for k in 0..4 {
        dst[k] = src[k] + dst[k] * keep;
    }
}

fn srgb(c: Color32) -> [f32; 4] {
    [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0, c.a() as f32 / 255.0]
}

/// The picture's color at (x, y) in its own pixels, smoothly between pixels.
fn sample(pic: &ColorImage, x: f32, y: f32) -> [f32; 4] {
    let [w, h] = pic.size;
    let x = x.clamp(0.0, (w - 1) as f32);
    let y = y.clamp(0.0, (h - 1) as f32);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let px = |x: usize, y: usize| srgb(pic.pixels[y * w + x]);
    let (a, b, c, d) = (px(x0, y0), px(x1, y0), px(x0, y1), px(x1, y1));
    let mut out = [0.0; 4];
    for k in 0..4 {
        let top = a[k] + (b[k] - a[k]) * fx;
        let bottom = c[k] + (d[k] - c[k]) * fx;
        out[k] = top + (bottom - top) * fy;
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn draw_card(
    canvas: &mut [[f32; 4]],
    side: usize,
    pic: &ColorImage,
    c: [f32; 2],
    half: [f32; 2],
    inner: [f32; 2],
    turn: f32,
    s: f32,
) {
    let (sin, cos) = turn.sin_cos();
    // Into the card's own (unturned) coordinates.
    let local = |x: f32, y: f32| [x * cos + y * sin, -x * sin + y * cos];
    let reach = (half[0].powi(2) + half[1].powi(2)).sqrt() + SHADOW * s + 4.0 * s;
    let x_range = ((c[0] - reach).max(0.0) as usize)..((c[0] + reach).min(side as f32) as usize);
    let y_range = ((c[1] - reach).max(0.0) as usize)..((c[1] + reach).min(side as f32) as usize);
    let frame = srgb(Color32::from_gray(244));
    for y in y_range {
        for x in x_range.clone() {
            let (px, py) = (x as f32 + 0.5 - c[0], y as f32 + 0.5 - c[1]);
            let dst = &mut canvas[y * side + x];

            // A soft shadow, a little below the card.
            let sp = local(px, py - 2.5 * s);
            let sd = rounded_rect(sp, half, RADIUS * s);
            let fade = (1.0 - (sd / (SHADOW * s)).max(0.0)).clamp(0.0, 1.0);
            let shadow = 0.4 * fade * fade;
            if shadow > 0.0 {
                over(dst, [0.0, 0.0, 0.0, shadow]);
            }

            let p = local(px, py);
            let cover = (0.5 - rounded_rect(p, half, RADIUS * s)).clamp(0.0, 1.0);
            if cover <= 0.0 {
                continue;
            }
            let color = if p[0].abs() <= inner[0] / 2.0 && p[1].abs() <= inner[1] / 2.0 {
                let u = (p[0] + inner[0] / 2.0) / inner[0] * pic.size[0] as f32 - 0.5;
                let v = (p[1] + inner[1] / 2.0) / inner[1] * pic.size[1] as f32 - 0.5;
                sample(pic, u, v)
            } else {
                frame
            };
            over(dst, [color[0] * cover, color[1] * cover, color[2] * cover, color[3] * cover]);
        }
    }
}

/// A pill in the accent color with the count in white, centered on `at`.
fn draw_badge(canvas: &mut [[f32; 4]], side: usize, text: &str, at: [f32; 2], accent: Color32, s: f32) {
    let mask = text_mask(text, (13.0 * s).round() as i32);
    let text_w = mask.as_ref().map_or(0.0, |m| m.size[0] as f32);
    let h = 22.0 * s;
    let w = h.max(text_w + 12.0 * s);
    // Kept inside the picture.
    let cx = at[0].min(side as f32 - w / 2.0 - 1.0);
    let cy = at[1].max(h / 2.0 + 1.0);
    let ring = 1.5 * s;
    let half = [w / 2.0 + ring, h / 2.0 + ring];
    let fill = srgb(accent);
    let white = [1.0, 1.0, 1.0, 1.0];
    for y in 0..side {
        for x in 0..side {
            let p = [x as f32 + 0.5 - cx, y as f32 + 0.5 - cy];
            if p[0].abs() > half[0] + 1.0 || p[1].abs() > half[1] + 1.0 {
                continue;
            }
            let dst = &mut canvas[y * side + x];
            let outer = (0.5 - rounded_rect(p, half, half[1])).clamp(0.0, 1.0);
            over(dst, white.map(|v| v * outer));
            let body = (0.5 - rounded_rect(p, [w / 2.0, h / 2.0], h / 2.0)).clamp(0.0, 1.0);
            over(dst, [fill[0] * body, fill[1] * body, fill[2] * body, body]);
        }
    }
    let Some(mask) = mask else { return };
    let x0 = (cx - mask.size[0] as f32 / 2.0).round() as isize;
    let y0 = (cy - mask.size[1] as f32 / 2.0).round() as isize;
    for my in 0..mask.size[1] {
        for mx in 0..mask.size[0] {
            let (x, y) = (x0 + mx as isize, y0 + my as isize);
            if x < 0 || y < 0 || x as usize >= side || y as usize >= side {
                continue;
            }
            let a = mask.cover[my * mask.size[0] + mx] as f32 / 255.0;
            over(&mut canvas[y as usize * side + x as usize], [a, a, a, a]);
        }
    }
}

/// How much of each pixel some text covers (0–255), drawn by Windows in Segoe UI.
struct TextMask {
    size: [usize; 2],
    cover: Vec<u8>,
}

#[cfg(windows)]
fn text_mask(text: &str, px: i32) -> Option<TextMask> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, CreateFontIndirectW, DeleteDC, DeleteObject, DrawTextW, GdiFlush,
        SelectObject, SetBkMode, SetTextColor, ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
        DIB_RGB_COLORS, DT_CALCRECT, DT_NOPREFIX, DT_SINGLELINE, LOGFONTW, TRANSPARENT,
    };
    use windows::Win32::Foundation::COLORREF;

    let mut wide: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: GDI calls on a memory DC this function creates and frees; every object
    // selected into it is put back before being deleted.
    unsafe {
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            return None;
        }
        let mut lf = LOGFONTW { lfHeight: -px, lfWeight: 600, lfQuality: ANTIALIASED_QUALITY, ..Default::default() };
        for (dst, src) in lf.lfFaceName.iter_mut().zip("Segoe UI".encode_utf16()) {
            *dst = src;
        }
        let font = CreateFontIndirectW(&lf);
        let old_font = SelectObject(dc, font.into());
        let mut rect = RECT::default();
        DrawTextW(dc, &mut wide, &mut rect, DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX);
        let (w, h) = (rect.right.max(1), rect.bottom.max(1));
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let mut result = None;
        if let Ok(bitmap) = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            let old_bitmap = SelectObject(dc, bitmap.into());
            SetTextColor(dc, COLORREF(0x00FF_FFFF));
            SetBkMode(dc, TRANSPARENT);
            DrawTextW(dc, &mut wide, &mut rect, DT_SINGLELINE | DT_NOPREFIX);
            let _ = GdiFlush();
            if !bits.is_null() {
                let pixels = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize);
                let cover = pixels.chunks_exact(4).map(|p| p[1]).collect();
                result = Some(TextMask { size: [w as usize, h as usize], cover });
            }
            SelectObject(dc, old_bitmap);
            let _ = DeleteObject(bitmap.into());
        }
        SelectObject(dc, old_font);
        let _ = DeleteObject(font.into());
        let _ = DeleteDC(dc);
        result
    }
}

#[cfg(not(windows))]
fn text_mask(_text: &str, _px: i32) -> Option<TextMask> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo(w: usize, h: usize, c: Color32) -> ColorImage {
        ColorImage::new([w, h], vec![c; w * h])
    }

    /// Three photos of twelve: a fanned stack with see-through corners, the front
    /// photo in the middle, a white frame, and the accent badge with "12" in white.
    #[test]
    fn draws_a_stack_with_a_count_badge() {
        let accent = Color32::from_rgb(10, 132, 255);
        let pics = [photo(150, 100, Color32::RED), photo(100, 150, Color32::GREEN), photo(120, 120, Color32::BLUE)];
        let img = stack(&pics, 12, accent, 1.0);
        let [w, h] = img.size;
        assert_eq!(img.bgra.len(), w * h * 4);
        let px = |x: usize, y: usize| {
            let i = (y * w + x) * 4;
            [img.bgra[i + 2], img.bgra[i + 1], img.bgra[i], img.bgra[i + 3]]
        };
        assert_eq!(px(0, h - 1)[3], 0, "corners are see-through");
        let center = [w / 2, h / 2 + 6];
        assert_eq!(px(center[0], center[1]), [255, 0, 0, 255], "the front photo is the dragged one");
        assert!((0..h as i32).contains(&img.hotspot[1]) && (0..w as i32).contains(&img.hotspot[0]));

        // The badge: accent-colored pixels and white text pixels near the front card's
        // top-right corner.
        let near_accent = |p: [u8; 4]| p[3] == 255 && p[0].abs_diff(10) < 8 && p[2].abs_diff(255) < 8 && p[1].abs_diff(132) < 8;
        let all: Vec<[u8; 4]> = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| px(x, y)).collect();
        assert!(all.iter().filter(|p| near_accent(**p)).count() > 100, "badge drawn");
        #[cfg(windows)]
        {
            let mask = text_mask("12", 13).expect("Windows draws text");
            assert!(mask.cover.iter().any(|&c| c > 200), "the count is drawn");
        }

        // To look at a stack of real photos (saved as a PNG):
        // $env:CULLERY_STACK_SAMPLES='F:\DCIM\100CANON'; $env:CULLERY_STACK_OUT='stack.png'
        if let (Ok(dir), Ok(out)) = (std::env::var("CULLERY_STACK_SAMPLES"), std::env::var("CULLERY_STACK_OUT")) {
            let files: Vec<_> = std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok().map(|e| e.path())).take(3).collect();
            let scale = 1.5;
            let pics: Vec<ColorImage> =
                files.iter().filter_map(|p| crate::loader::quick_picture(p, [165, 165])).collect();
            let img = stack(&pics, 27, accent, scale);
            let rgba: Vec<u8> = img.bgra.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
            image::RgbaImage::from_raw(img.size[0] as u32, img.size[1] as u32, rgba).unwrap().save(out).unwrap();
        }

        // One photo: no badge.
        let single = stack(&pics[..1], 1, accent, 1.0);
        let any_accent = single.bgra.chunks_exact(4).any(|p| near_accent([p[2], p[1], p[0], p[3]]));
        assert!(!any_accent);
    }
}
