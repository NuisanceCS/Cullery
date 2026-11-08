//! Finding photos in a folder and pairing each camera image with its RAW.
//!
//! Cameras write the JPG (or HEIC) and the RAW of one shot with the same file name
//! (`IMG_4821.JPG` + `IMG_4821.CR3`), so pairing is done by name, ignoring case.
//! Other image types (PNG, GIF, WebP, TIFF, BMP) are never paired: each is its own shot.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::raw;

/// Camera images that pair with a RAW of the same name.
const CAMERA_IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "heic", "heif"];
/// Images shown on their own, never paired.
const OTHER_IMAGE_EXTENSIONS: &[&str] = &["png", "gif", "webp", "tif", "tiff", "bmp", "avif"];

/// One shot: an image, a RAW, or both.
#[derive(Clone, Debug)]
pub struct Shot {
    /// File name without extension, as found on disk (e.g. `IMG_4821`).
    pub name: String,
    /// Identifies the shot within its folder (picks are saved under it): the lowercase
    /// name for camera shots, the lowercase file name for unpaired images.
    pub key: String,
    /// The viewable image: JPG, HEIC, PNG, GIF, …
    pub image: Option<PathBuf>,
    pub raw: Option<PathBuf>,
}

impl Shot {
    pub fn files(&self) -> impl Iterator<Item = &PathBuf> {
        self.image.iter().chain(self.raw.iter())
    }

    pub fn image_name(&self) -> String {
        file_name(self.image.as_deref()).unwrap_or_else(|| format!("{}.JPG", self.name))
    }

    pub fn raw_name(&self) -> String {
        file_name(self.raw.as_deref()).unwrap_or_else(|| format!("{}.RAW", self.name))
    }

    /// The image's type in capitals, e.g. "JPG", "PNG", "HEIC" ("JPEG" shows as "JPG").
    pub fn image_kind(&self) -> String {
        match extension(self.image.as_deref()).as_deref() {
            Some("jpeg") | None => "JPG".into(),
            Some("tiff") => "TIF".into(),
            Some(ext) => ext.to_uppercase(),
        }
    }

    /// The RAW file's type in capitals, e.g. "CR3".
    pub fn raw_kind(&self) -> String {
        extension(self.raw.as_deref()).map(|e| e.to_uppercase()).unwrap_or_else(|| "RAW".into())
    }

    /// A camera image (JPG/HEIC) that would normally come with a RAW.
    pub fn is_camera_image(&self) -> bool {
        extension(self.image.as_deref()).is_some_and(|e| CAMERA_IMAGE_EXTENSIONS.contains(&e.as_str()))
    }

    /// The file to show: the image, or for RAW-only shots, the RAW's embedded preview.
    pub fn display_path(&self) -> Option<&PathBuf> {
        self.image.as_ref().or(self.raw.as_ref())
    }
}

fn file_name(path: Option<&Path>) -> Option<String> {
    path?.file_name()?.to_str().map(str::to_owned)
}

fn extension(path: Option<&Path>) -> Option<String> {
    Some(path?.extension()?.to_str()?.to_ascii_lowercase())
}

/// Lists the shots in `dir` (not its subfolders), sorted alphabetically by name.
pub fn scan(dir: &Path) -> io::Result<Vec<Shot>> {
    let mut shots: BTreeMap<String, Shot> = BTreeMap::new();
    for entry in std::fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        let (Some(stem), Some(ext), Some(file)) = (
            path.file_stem().and_then(|s| s.to_str()),
            path.extension().and_then(|e| e.to_str()),
            path.file_name().and_then(|f| f.to_str()),
        ) else {
            continue;
        };
        // macOS leaves "._IMG_0001.JPG" metadata files on cards; they aren't photos.
        if stem.starts_with("._") {
            continue;
        }
        let ext = ext.to_ascii_lowercase();
        let is_camera = CAMERA_IMAGE_EXTENSIONS.contains(&ext.as_str());
        let is_other = OTHER_IMAGE_EXTENSIONS.contains(&ext.as_str());
        let is_raw = raw::is_raw_extension(&ext);
        if !is_camera && !is_other && !is_raw {
            continue;
        }
        let key = if is_other { file.to_lowercase() } else { stem.to_lowercase() };
        let shot = shots.entry(key.clone()).or_insert_with(|| Shot {
            name: stem.to_owned(),
            key,
            image: None,
            raw: None,
        });
        if is_raw {
            shot.raw = Some(path);
        } else {
            shot.image = Some(path);
        }
    }
    Ok(shots.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_camera_images_with_raw_and_keeps_others_separate() {
        let dir = std::env::temp_dir().join(format!("cullery-scan-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "IMG_0002.JPG",
            "IMG_0002.CR3",
            "IMG_0001.jpg",
            "img_0001.cr3",
            "IMG_0003.CR3",
            "IMG_0004.JPG",
            "DSC_0005.NEF",
            "IMG_0006.HEIC",
            "IMG_0006.CR3",
            "IMG_0001.png",
            "anim.gif",
            "notes.txt",
            "._IMG_0002.JPG",
        ] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }

        let shots = scan(&dir).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        let summary: Vec<(String, String, bool)> =
            shots.iter().map(|s| (s.key.clone(), s.image_kind(), s.raw.is_some())).collect();
        assert_eq!(
            summary,
            vec![
                ("anim.gif".into(), "GIF".into(), false),
                ("dsc_0005".into(), "JPG".into(), true),
                ("img_0001".into(), "JPG".into(), true),
                ("img_0001.png".into(), "PNG".into(), false),
                ("img_0002".into(), "JPG".into(), true),
                ("img_0003".into(), "JPG".into(), true),
                ("img_0004".into(), "JPG".into(), false),
                ("img_0006".into(), "HEIC".into(), true),
            ]
        );
        assert!(shots[0].image.is_some() && !shots[0].is_camera_image());
        assert!(shots[1].image.is_none(), "DSC_0005 is RAW-only");
        assert!(shots[7].is_camera_image());
    }
}
