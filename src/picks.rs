//! Remembers which shots in a folder are picked, between runs.
//!
//! Picks are stored per source folder in %APPDATA%\Cullery\picks\, as a list of shot
//! names and file sizes. They're kept apart from the export folder's contents so a
//! Quick Export (copying without picking) doesn't count as a pick.
//!
//! The size matters because a folder's location isn't unique: every memory card can be
//! `F:\DCIM\100CANON`, with photos of the same names. A photo only counts as picked if
//! it's the same size as the one that was picked.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// A remembered pick: the shot's name (lowercase) and its size in bytes. Picks saved by
/// older versions have no size.
pub type Pick = (String, Option<u64>);

pub fn file_for(folder: &Path) -> Option<PathBuf> {
    let key = folder.to_string_lossy().to_lowercase();
    Some(crate::settings::data_dir()?.join("picks").join(format!("{:016x}.txt", fnv1a(key.as_bytes()))))
}

/// Stable hash (unlike std's), so the same folder always maps to the same file.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3))
}

/// The picks saved for `folder`, or `None` if it has never been saved.
pub fn load(folder: &Path) -> Option<HashSet<Pick>> {
    let text = std::fs::read_to_string(file_for(folder)?).ok()?;
    Some(text.lines().filter(|l| !l.is_empty() && !l.starts_with('#')).map(parse_line).collect())
}

fn parse_line(line: &str) -> Pick {
    match line.rsplit_once('\t').and_then(|(name, size)| Some((name, size.parse().ok()?))) {
        Some((name, size)) => (name.to_lowercase(), Some(size)),
        None => (line.to_lowercase(), None),
    }
}

pub fn save(folder: &Path, picks: impl Iterator<Item = Pick>) {
    let Some(path) = file_for(folder) else { return };
    let mut lines: Vec<String> = picks
        .map(|(name, size)| match size {
            Some(size) => format!("{name}\t{size}"),
            None => name,
        })
        .collect();
    lines.sort_unstable();
    let mut text = format!("# Cullery picks for {}\n", folder.display());
    for line in lines {
        text += &line;
        text.push('\n');
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_new_and_old_lines() {
        assert_eq!(parse_line("100canon/img_1\t5120"), ("100canon/img_1".to_string(), Some(5120)));
        assert_eq!(parse_line("IMG_1"), ("img_1".to_string(), None));
    }
}
