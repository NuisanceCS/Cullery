//! What the viewer's info bar says about a photo: when and with what it was taken,
//! from the EXIF data cameras and phones write into the file.

use std::path::Path;

/// The camera details found in a photo. Anything the file doesn't say is `None`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PhotoInfo {
    /// "8 Nov 2026, 14:03", in the camera's own clock.
    pub taken: Option<String>,
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub iso: Option<String>,
    pub shutter: Option<String>,
    pub aperture: Option<String>,
    pub focal: Option<String>,
    /// Pixel size as the file records it.
    pub size: Option<[u32; 2]>,
}

/// Reads the camera details of the file at `path`; a file without any gives the default.
pub fn read(path: &Path) -> PhotoInfo {
    let Ok(file) = std::fs::File::open(path) else { return PhotoInfo::default() };
    let Ok(exif) = exif::Reader::new().read_from_container(&mut std::io::BufReader::new(file)) else {
        return PhotoInfo::default();
    };
    let field = |tag: exif::Tag| exif.get_field(tag, exif::In::PRIMARY);
    let text = |tag: exif::Tag| {
        field(tag).and_then(|f| match &f.value {
            exif::Value::Ascii(parts) => parts.first().map(|b| String::from_utf8_lossy(b).trim().trim_end_matches('\0').trim().to_string()),
            _ => None,
        })
        .filter(|s| !s.is_empty())
    };
    let rational = |tag: exif::Tag| match field(tag).map(|f| &f.value) {
        Some(exif::Value::Rational(v)) => v.first().filter(|r| r.denom != 0).map(|r| (r.num as f64, r.denom as f64)),
        _ => None,
    };
    let number = |tag: exif::Tag| field(tag).and_then(|f| f.value.get_uint(0));

    let make = text(exif::Tag::Make);
    let camera = match (make, text(exif::Tag::Model)) {
        // Most models already start with the maker ("Canon EOS R6").
        (Some(make), Some(model)) if !model.to_lowercase().starts_with(&first_word(&make).to_lowercase()) => {
            Some(format!("{} {model}", first_word(&make)))
        }
        (_, Some(model)) => Some(model),
        (make, None) => make,
    };
    PhotoInfo {
        taken: text(exif::Tag::DateTimeOriginal).or_else(|| text(exif::Tag::DateTime)).and_then(|t| exif_date(&t)),
        camera,
        lens: text(exif::Tag::LensModel),
        iso: number(exif::Tag::PhotographicSensitivity).map(|n| format!("ISO {n}")),
        shutter: rational(exif::Tag::ExposureTime).map(|(n, d)| shutter_text(n / d)),
        aperture: rational(exif::Tag::FNumber).map(|(n, d)| format!("f/{}", trim_number(n / d))),
        focal: rational(exif::Tag::FocalLength).map(|(n, d)| format!("{} mm", trim_number(n / d))),
        size: number(exif::Tag::PixelXDimension).zip(number(exif::Tag::PixelYDimension)).map(|(w, h)| [w, h]),
    }
}

fn first_word(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or(s)
}

/// "1/250 s" for fast shutters, "2 s" or "0.5 s" for slow ones.
fn shutter_text(seconds: f64) -> String {
    if seconds <= 0.0 {
        return "–".into();
    }
    if seconds < 0.4 {
        format!("1/{} s", (1.0 / seconds).round())
    } else {
        format!("{} s", trim_number(seconds))
    }
}

/// 2.8 stays 2.8, 4.0 becomes 4, 1.7999 becomes 1.8.
fn trim_number(x: f64) -> String {
    let rounded = (x * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 { format!("{}", rounded as i64) } else { format!("{rounded}") }
}

/// "2026:11:08 14:03:22" (how EXIF writes dates) as "8 Nov 2026, 14:03".
fn exif_date(text: &str) -> Option<String> {
    let (date, time) = text.split_once(' ')?;
    let mut d = date.split(':').map(|p| p.parse::<u32>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    if y == 0 || !(1..=12).contains(&m) {
        return None;
    }
    let hm: String = time.chars().take(5).collect();
    Some(format!("{day} {} {y}, {hm}", MONTHS[m as usize - 1]))
}

pub const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_camera_settings_as_photographers_write_them() {
        assert_eq!(shutter_text(1.0 / 250.0), "1/250 s");
        assert_eq!(shutter_text(0.5), "0.5 s");
        assert_eq!(shutter_text(2.0), "2 s");
        assert_eq!(trim_number(2.8), "2.8");
        assert_eq!(trim_number(4.0), "4");
        assert_eq!(exif_date("2026:11:08 14:03:22").as_deref(), Some("8 Nov 2026, 14:03"));
        assert_eq!(exif_date("0000:00:00 00:00:00"), None);
    }

    /// Reads a real camera file:
    /// $env:CULLERY_EXIF_SAMPLE='F:\DCIM\100CANON\IMG_0001.JPG'; cargo test --release real_camera_file -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_camera_file() {
        let Some(path) = std::env::var_os("CULLERY_EXIF_SAMPLE") else { return };
        let info = read(Path::new(&path));
        println!("{info:#?}");
        assert!(info.camera.is_some());
    }

    /// A JPG with camera details written into it, as a camera would.
    #[test]
    fn reads_what_a_camera_writes() {
        use exif::{Field, In, Rational, Tag, Value};
        let ascii = |s: &str| Value::Ascii(vec![s.as_bytes().to_vec()]);
        let fields = [
            Field { tag: Tag::Make, ifd_num: In::PRIMARY, value: ascii("Canon") },
            Field { tag: Tag::Model, ifd_num: In::PRIMARY, value: ascii("Canon EOS R6") },
            Field { tag: Tag::DateTimeOriginal, ifd_num: In::PRIMARY, value: ascii("2026:11:08 14:03:22") },
            Field { tag: Tag::LensModel, ifd_num: In::PRIMARY, value: ascii("RF24-70mm F2.8 L IS USM") },
            Field { tag: Tag::ExposureTime, ifd_num: In::PRIMARY, value: Value::Rational(vec![Rational { num: 1, denom: 250 }]) },
            Field { tag: Tag::FNumber, ifd_num: In::PRIMARY, value: Value::Rational(vec![Rational { num: 28, denom: 10 }]) },
            Field { tag: Tag::PhotographicSensitivity, ifd_num: In::PRIMARY, value: Value::Short(vec![400]) },
            Field { tag: Tag::FocalLength, ifd_num: In::PRIMARY, value: Value::Rational(vec![Rational { num: 50, denom: 1 }]) },
        ];
        let mut writer = exif::experimental::Writer::new();
        for f in &fields {
            writer.push_field(f);
        }
        let mut tiff = std::io::Cursor::new(Vec::new());
        writer.write(&mut tiff, false).unwrap();

        // A JPG's start, the details block cameras write, and its end: enough to read.
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend(tiff.into_inner());
        let mut jpg = vec![0xFF, 0xD8, 0xFF, 0xE1];
        jpg.extend(((app1.len() + 2) as u16).to_be_bytes());
        jpg.extend(app1);
        jpg.extend([0xFF, 0xD9]);
        let path = std::env::temp_dir().join(format!("cullery-info-camera-{}.jpg", std::process::id()));
        std::fs::write(&path, jpg).unwrap();

        let info = read(&path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(info.camera.as_deref(), Some("Canon EOS R6"));
        assert_eq!(info.taken.as_deref(), Some("8 Nov 2026, 14:03"));
        assert_eq!(info.lens.as_deref(), Some("RF24-70mm F2.8 L IS USM"));
        assert_eq!(info.shutter.as_deref(), Some("1/250 s"));
        assert_eq!(info.aperture.as_deref(), Some("f/2.8"));
        assert_eq!(info.iso.as_deref(), Some("ISO 400"));
        assert_eq!(info.focal.as_deref(), Some("50 mm"));
    }

    #[test]
    fn a_file_without_exif_has_no_details() {
        let path = std::env::temp_dir().join(format!("cullery-info-{}.png", std::process::id()));
        image::RgbaImage::new(4, 3).save(&path).unwrap();
        let info = read(&path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(info, PhotoInfo::default());
    }
}
