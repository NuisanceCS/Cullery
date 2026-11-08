//! Remembers which shots in a folder are picked, between runs.
//!
//! Picks are stored per source folder in %APPDATA%\Cullery\picks\, as a list of shot
//! names. They're kept apart from the export folder's contents so a Quick Export
//! (copying without picking) doesn't count as a pick.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub fn file_for(folder: &Path) -> Option<PathBuf> {
    let key = folder.to_string_lossy().to_lowercase();
    Some(crate::settings::data_dir()?.join("picks").join(format!("{:016x}.txt", fnv1a(key.as_bytes()))))
}

/// Stable hash (unlike std's), so the same folder always maps to the same file.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3))
}

/// Picked shot names (lowercase) for `folder`, or `None` if it has never been saved.
pub fn load(folder: &Path) -> Option<HashSet<String>> {
    let text = std::fs::read_to_string(file_for(folder)?).ok()?;
    Some(
        text.lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(str::to_lowercase)
            .collect(),
    )
}

pub fn save<'a>(folder: &Path, names: impl Iterator<Item = &'a str>) {
    let Some(path) = file_for(folder) else { return };
    let mut names: Vec<&str> = names.collect();
    names.sort_unstable();
    let mut text = format!("# Cullery picks for {}\n", folder.display());
    for name in names {
        text += name;
        text.push('\n');
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, text);
}
