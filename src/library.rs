//! Finding photos in a folder and pairing each camera image with its RAW.
//!
//! Cameras write the JPG (or HEIC) and the RAW of one shot with the same file name
//! (`IMG_4821.JPG` + `IMG_4821.CR3`), so pairing is done by name, ignoring case.
//! Other image types (PNG, GIF, WebP, TIFF, BMP) are never paired: each is its own shot.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::time::SystemTime;

use crate::raw;

/// Camera images that pair with a RAW of the same name.
pub const CAMERA_IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "heic", "heif"];
/// Images shown on their own, never paired.
const OTHER_IMAGE_EXTENSIONS: &[&str] = &["png", "gif", "webp", "tif", "tiff", "bmp", "avif"];

/// What the grid is ordered by, like File Explorer's Sort by.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortBy {
    Name,
    Date,
    Type,
    Size,
}

impl SortBy {
    pub const ALL: [SortBy; 4] = [SortBy::Name, SortBy::Date, SortBy::Type, SortBy::Size];

    pub fn key(self) -> &'static str {
        match self {
            SortBy::Name => "name",
            SortBy::Date => "date",
            SortBy::Type => "type",
            SortBy::Size => "size",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SortBy::Name => "Name",
            SortBy::Date => "Date modified",
            SortBy::Type => "Type",
            SortBy::Size => "Size",
        }
    }
}

/// Puts shots in order, with equal ones by name. Sizes and dates are those of the file
/// the grid shows: the image, or the RAW if there's no image.
pub fn sort(shots: &mut [Shot], by: SortBy, descending: bool) {
    shots.sort_by(|a, b| {
        let first = match by {
            SortBy::Name => Ordering::Equal,
            SortBy::Date => a.modified.cmp(&b.modified),
            SortBy::Type => a.type_name().cmp(&b.type_name()),
            SortBy::Size => a.size.cmp(&b.size),
        };
        let order = first.then_with(|| natural_cmp(&a.name, &b.name)).then_with(|| a.key.cmp(&b.key));
        if descending { order.reverse() } else { order }
    });
}

/// Compares names as File Explorer does: ignoring case, with runs of digits compared
/// as numbers, so `IMG_9` comes before `IMG_10`.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    fn number(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
        let mut digits = String::new();
        while let Some(c) = chars.next_if(char::is_ascii_digit) {
            digits.push(c);
        }
        let trimmed = digits.trim_start_matches('0');
        trimmed.to_string()
    }
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let (m, n) = (number(&mut x), number(&mut y));
                let order = m.len().cmp(&n.len()).then_with(|| m.cmp(&n));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(c), Some(d)) => {
                let order = c.to_lowercase().cmp(d.to_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
                x.next();
                y.next();
            }
        }
    }
}

/// One shot: an image, a RAW, or both.
#[derive(Clone, Debug, Default)]
pub struct Shot {
    /// File name without extension, as found on disk (e.g. `IMG_4821`).
    pub name: String,
    /// Identifies the shot within its folder (picks are saved under it): the lowercase
    /// name for camera shots, the lowercase file name for unpaired images.
    pub key: String,
    /// The viewable image: JPG, HEIC, PNG, GIF, …
    pub image: Option<PathBuf>,
    pub raw: Option<PathBuf>,
    /// When the shown file was last modified, and its size in bytes (for sorting).
    pub modified: Option<SystemTime>,
    pub size: u64,
    /// The size of the RAW file, in bytes.
    pub raw_size: u64,
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

    /// The type of the file the grid shows.
    fn type_name(&self) -> String {
        if self.image.is_some() { self.image_kind() } else { self.raw_kind() }
    }

    /// A camera image (JPG/HEIC) that would normally come with a RAW.
    pub fn is_camera_image(&self) -> bool {
        extension(self.image.as_deref()).is_some_and(|e| CAMERA_IMAGE_EXTENSIONS.contains(&e.as_str()))
    }

    /// What the viewer says about this shot's files, like
    /// "JPEG Paired with CR3 - 1 MB and 12 MB".
    pub fn summary(&self) -> String {
        let kind = match self.image_kind().as_str() {
            "JPG" => "JPEG".to_string(),
            other => other.to_string(),
        };
        match (&self.image, &self.raw) {
            (Some(_), Some(_)) => {
                format!("{kind} Paired with {} - {} and {}", self.raw_kind(), size_text(self.size), size_text(self.raw_size))
            }
            (Some(_), None) if self.is_camera_image() => format!("{kind}, no RAW - {}", size_text(self.size)),
            (Some(_), None) => format!("{kind} image - {}", size_text(self.size)),
            _ => format!("{} RAW only - {}", self.raw_kind(), size_text(self.raw_size)),
        }
    }

    /// The file to show: the image, or for RAW-only shots, the RAW's embedded preview.
    pub fn display_path(&self) -> Option<&PathBuf> {
        self.image.as_ref().or(self.raw.as_ref())
    }
}

/// A file size for reading at a glance: "850 KB", "12 MB", "1.2 GB".
fn size_text(bytes: u64) -> String {
    let kb = bytes as f64 / 1024.0;
    let mb = kb / 1024.0;
    if mb < 1.0 {
        format!("{} KB", kb.round().max(1.0))
    } else if mb < 1024.0 {
        format!("{} MB", mb.round())
    } else {
        format!("{:.1} GB", mb / 1024.0)
    }
}

fn file_name(path: Option<&Path>) -> Option<String> {
    path?.file_name()?.to_str().map(str::to_owned)
}

fn extension(path: Option<&Path>) -> Option<String> {
    Some(path?.extension()?.to_str()?.to_ascii_lowercase())
}

/// Whether `dir` is a whole drive. Opening one takes in the folders inside it too.
pub fn is_drive_root(dir: &Path) -> bool {
    if cfg!(target_os = "macos") {
        dir.parent() == Some(Path::new("/Volumes"))
    } else {
        dir.parent().is_none() && dir.has_root()
    }
}

/// Lists the shots in `dir` (not its subfolders), sorted alphabetically by name.
pub fn scan(dir: &Path) -> io::Result<Vec<Shot>> {
    let files = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok().filter(|m| m.is_file()).map(|m| (e.path(), m)))
        .collect();
    Ok(pair_up(dir, files))
}

/// Folders the system keeps for itself on a drive.
fn is_system_folder(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    name.starts_with('.') || name.starts_with('$') || name.eq_ignore_ascii_case("System Volume Information")
}

/// Lists the shots in `dir` and every folder inside it, except `skip`. Each file seen
/// adds to `checked`; setting `stop` ends the search early.
pub fn scan_with_subfolders(dir: &Path, skip: Option<&Path>, checked: &AtomicUsize, stop: &AtomicBool) -> io::Result<Vec<Shot>> {
    let mut files = Vec::new();
    let mut folders = vec![dir.to_path_buf()];
    while let Some(folder) = folders.pop() {
        if stop.load(AtomicOrdering::Relaxed) {
            break;
        }
        let entries = match std::fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(e) if folder == dir => return Err(e),
            // One unreadable folder shouldn't stop the rest of the drive.
            Err(_) => continue,
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let Ok(meta) = entry.metadata() else { continue };
            let path = entry.path();
            if meta.is_file() {
                files.push((path, meta));
                checked.fetch_add(1, AtomicOrdering::Relaxed);
            } else if meta.is_dir() && !is_system_folder(&entry.file_name()) && skip != Some(path.as_path()) {
                folders.push(path);
            }
        }
    }
    Ok(pair_up(dir, files))
}

/// Turns the files found under `dir` into shots. Only files in the same folder pair up.
fn pair_up(dir: &Path, mut files: Vec<(PathBuf, std::fs::Metadata)>) -> Vec<Shot> {
    // A set order, so the same file pairs up every time (see `taken` below).
    files.sort_by_key(|(p, _)| p.to_string_lossy().to_lowercase());

    let mut shots: BTreeMap<String, Shot> = BTreeMap::new();
    for (path, meta) in files {
        // Which subfolder it's in, so IMG_1 in two folders stays two shots.
        let folder = path.parent().and_then(|p| p.strip_prefix(dir).ok()).map(|p| p.to_string_lossy().to_lowercase().replace('\\', "/"));
        let prefix = folder.filter(|f| !f.is_empty()).map(|f| f + "/").unwrap_or_default();
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
        let stem_key = format!("{prefix}{}", stem.to_lowercase());
        // A second image or RAW of the same name (IMG_1.JPG + IMG_1.HEIC) is its own shot.
        let taken = shots.get(&stem_key).is_some_and(|s| if is_raw { s.raw.is_some() } else { s.image.is_some() });
        let key = if is_other || taken { format!("{prefix}{}", file.to_lowercase()) } else { stem_key };
        let shot = shots.entry(key.clone()).or_insert_with(|| Shot {
            name: stem.to_owned(),
            key,
            ..Default::default()
        });
        if !is_raw || shot.image.is_none() {
            shot.modified = meta.modified().ok();
            shot.size = meta.len();
        }
        if is_raw {
            shot.raw = Some(path);
            shot.raw_size = meta.len();
        } else {
            shot.image = Some(path);
        }
    }
    shots.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_search_finds_subfolders_and_keeps_same_names_apart() {
        let dir = std::env::temp_dir().join(format!("cullery-drive-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for folder in ["", "100CANON", "101CANON", "Selects", ".Trashes", "Exports"] {
            std::fs::create_dir_all(dir.join(folder)).unwrap();
        }
        for file in [
            "top.jpg",
            "100CANON/IMG_1.JPG",
            "100CANON/IMG_1.CR3",
            "101CANON/IMG_1.JPG",
            ".Trashes/hidden.jpg",
            "Exports/copy.jpg",
        ] {
            std::fs::write(dir.join(file), b"x").unwrap();
        }

        let checked = AtomicUsize::new(0);
        let shots = scan_with_subfolders(&dir, Some(&dir.join("Exports")), &checked, &AtomicBool::new(false)).unwrap();
        let mut found: Vec<(String, bool)> = shots.iter().map(|s| (s.key.clone(), s.raw.is_some())).collect();
        found.sort();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            found,
            [("100canon/img_1".to_string(), true), ("101canon/img_1".to_string(), false), ("top".to_string(), false)]
        );
        assert_eq!(checked.load(AtomicOrdering::Relaxed), 4);
    }

    #[cfg(windows)]
    #[test]
    fn only_a_whole_drive_counts_as_one() {
        assert!(is_drive_root(Path::new(r"F:\")));
        assert!(!is_drive_root(Path::new(r"F:\DCIM")));
    }

    #[test]
    fn summary_names_each_files_type_and_size() {
        const MB: u64 = 1024 * 1024;
        let shot = |image: Option<&str>, raw: Option<&str>, size, raw_size| Shot {
            image: image.map(PathBuf::from),
            raw: raw.map(PathBuf::from),
            size,
            raw_size,
            ..Default::default()
        };
        assert_eq!(shot(Some("a.JPG"), Some("a.CR3"), MB, 12 * MB).summary(), "JPEG Paired with CR3 - 1 MB and 12 MB");
        assert_eq!(shot(Some("a.png"), None, 2 * MB, 0).summary(), "PNG image - 2 MB");
        assert_eq!(shot(Some("a.jpg"), None, 850 * 1024, 0).summary(), "JPEG, no RAW - 850 KB");
        assert_eq!(shot(None, Some("a.cr3"), 12 * MB, 12 * MB).summary(), "CR3 RAW only - 12 MB");
        assert_eq!(size_text(1536 * MB), "1.5 GB");
    }

    #[test]
    fn names_sort_like_file_explorer() {
        let mut names = ["IMG_10", "img_9", "IMG_100", "IMG_2", "photo", "IMG_02b"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, ["IMG_2", "IMG_02b", "img_9", "IMG_10", "IMG_100", "photo"]);
        assert_eq!(natural_cmp("IMG_0001", "img_1"), Ordering::Equal);
    }

    #[test]
    fn sorts_by_each_choice_and_direction() {
        use std::time::Duration;
        let shot = |name: &str, ext: &str, secs: u64, size: u64| Shot {
            name: name.into(),
            key: name.to_lowercase(),
            image: Some(format!("{name}.{ext}").into()),
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs)),
            size,
            ..Default::default()
        };
        let mut shots = vec![shot("IMG_10", "JPG", 30, 5), shot("IMG_2", "PNG", 10, 9), shot("IMG_3", "JPG", 20, 7)];
        let order = |shots: &[Shot]| shots.iter().map(|s| s.name.clone()).collect::<Vec<_>>();

        sort(&mut shots, SortBy::Name, false);
        assert_eq!(order(&shots), ["IMG_2", "IMG_3", "IMG_10"]);
        sort(&mut shots, SortBy::Name, true);
        assert_eq!(order(&shots), ["IMG_10", "IMG_3", "IMG_2"]);
        sort(&mut shots, SortBy::Date, false);
        assert_eq!(order(&shots), ["IMG_2", "IMG_3", "IMG_10"]);
        sort(&mut shots, SortBy::Size, true);
        assert_eq!(order(&shots), ["IMG_2", "IMG_3", "IMG_10"]);
        // Same type: by name.
        sort(&mut shots, SortBy::Type, false);
        assert_eq!(order(&shots), ["IMG_3", "IMG_10", "IMG_2"]);
    }

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
            "IMG_0007.JPG",
            "IMG_0007.HEIC",
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
                ("img_0007".into(), "HEIC".into(), false),
                ("img_0007.jpg".into(), "JPG".into(), false),
            ]
        );
        assert!(shots[0].image.is_some() && !shots[0].is_camera_image());
        assert!(shots[1].image.is_none(), "DSC_0005 is RAW-only");
        assert!(shots[7].is_camera_image());
    }
}
