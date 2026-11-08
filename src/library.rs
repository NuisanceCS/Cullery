//! Finding photos in a folder and pairing each JPG with its RAW.
//!
//! Canon writes the JPG and the CR3 of one shot with the same file name
//! (`IMG_4821.JPG` + `IMG_4821.CR3`), so pairing is done by name, ignoring case.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

const JPG_EXTENSIONS: &[&str] = &["jpg", "jpeg"];
const RAW_EXTENSIONS: &[&str] = &["cr3", "cr2"];

/// One shot: a JPG, a RAW, or both.
#[derive(Clone, Debug)]
pub struct Shot {
    /// File name without extension, as found on disk (e.g. `IMG_4821`).
    pub name: String,
    pub jpg: Option<PathBuf>,
    pub raw: Option<PathBuf>,
}

impl Shot {
    pub fn files(&self) -> impl Iterator<Item = &PathBuf> {
        self.jpg.iter().chain(self.raw.iter())
    }

    pub fn jpg_name(&self) -> String {
        file_name(self.jpg.as_deref()).unwrap_or_else(|| format!("{}.JPG", self.name))
    }

    pub fn raw_name(&self) -> String {
        file_name(self.raw.as_deref()).unwrap_or_else(|| format!("{}.CR3", self.name))
    }
}

fn file_name(path: Option<&Path>) -> Option<String> {
    path?.file_name()?.to_str().map(str::to_owned)
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
        let (Some(stem), Some(ext)) = (
            path.file_stem().and_then(|s| s.to_str()),
            path.extension().and_then(|e| e.to_str()),
        ) else {
            continue;
        };
        // macOS leaves "._IMG_0001.JPG" metadata files on cards; they aren't photos.
        if stem.starts_with("._") {
            continue;
        }
        let ext = ext.to_ascii_lowercase();
        let is_jpg = JPG_EXTENSIONS.contains(&ext.as_str());
        let is_raw = RAW_EXTENSIONS.contains(&ext.as_str());
        if !is_jpg && !is_raw {
            continue;
        }
        let shot = shots.entry(stem.to_lowercase()).or_insert_with(|| Shot {
            name: stem.to_owned(),
            jpg: None,
            raw: None,
        });
        if is_jpg {
            shot.jpg = Some(path);
        } else {
            shot.raw = Some(path);
        }
    }
    Ok(shots.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_jpg_and_raw_by_name() {
        let dir = std::env::temp_dir().join(format!("culler-scan-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "IMG_0002.JPG",
            "IMG_0002.CR3",
            "IMG_0001.jpg",
            "img_0001.cr3",
            "IMG_0003.CR3",
            "IMG_0004.JPG",
            "notes.txt",
            "._IMG_0002.JPG",
        ] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }

        let shots = scan(&dir).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        let summary: Vec<(String, bool, bool)> = shots
            .iter()
            .map(|s| (s.name.to_uppercase(), s.jpg.is_some(), s.raw.is_some()))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("IMG_0001".into(), true, true),
                ("IMG_0002".into(), true, true),
                ("IMG_0003".into(), false, true),
                ("IMG_0004".into(), true, false),
            ]
        );
    }
}
