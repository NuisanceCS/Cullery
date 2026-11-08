//! Remembers the export folder, recent folders and view options between runs,
//! in a small text file under %APPDATA%\Cullery.

use std::path::PathBuf;
use std::sync::OnceLock;

use crate::library::SortBy;
use crate::theme::ThemeId;

/// `%APPDATA%\Cullery`, where settings and picks live.
///
/// The app was called Culler at first; the first run under the new name moves that
/// folder over so settings, recent folders and picks carry across.
pub fn data_dir() -> Option<PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let base = PathBuf::from(std::env::var_os("APPDATA")?);
        let dir = base.join("Cullery");
        let old = base.join("Culler");
        if !dir.exists() && old.is_dir() {
            let _ = std::fs::rename(&old, &dir);
        }
        Some(dir)
    })
    .clone()
}

const MAX_RECENT: usize = 8;

pub struct Settings {
    pub export_dir: Option<PathBuf>,
    pub recent: Vec<PathBuf>,
    pub show_raw: bool,
    pub tile_width: f32,
    pub drag_files: DragFiles,
    pub theme: ThemeId,
    /// Picking also copies the photo to the export folder (off: picks are just marked).
    pub export_on_pick: bool,
    /// Which files Export on Pick and Quick Export copy.
    pub export_files: DragFiles,
    pub sort_by: SortBy,
    pub sort_descending: bool,
    /// The values as last loaded or saved by this window, to tell which ones it changed.
    saved: Snapshot,
}

/// A copy of the saved values.
#[derive(Clone, PartialEq, Debug)]
struct Snapshot {
    export_dir: Option<PathBuf>,
    recent: Vec<PathBuf>,
    show_raw: bool,
    tile_width: f32,
    drag_files: DragFiles,
    theme: ThemeId,
    export_on_pick: bool,
    export_files: DragFiles,
    sort_by: SortBy,
    sort_descending: bool,
}

/// Which of a shot's files to use, when dragging photos out or copying them to the
/// export folder.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DragFiles {
    /// The RAW (or the JPG, for shots without one). For editing in Lightroom / LightCraft.
    Raw,
    /// The JPG (or the RAW, for shots without one).
    Jpg,
    Both,
}

impl DragFiles {
    pub const ALL: [DragFiles; 3] = [DragFiles::Raw, DragFiles::Jpg, DragFiles::Both];

    fn key(self) -> &'static str {
        match self {
            DragFiles::Raw => "raw",
            DragFiles::Jpg => "jpg",
            DragFiles::Both => "both",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DragFiles::Raw => "RAW files",
            DragFiles::Jpg => "JPG files",
            DragFiles::Both => "JPG + RAW",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            DragFiles::Raw => "Best for editing in Lightroom or LightCraft. Shots without a RAW send their JPG.",
            DragFiles::Jpg => "For sharing or quick edits. Shots without a JPG send their RAW.",
            DragFiles::Both => "Every file of each shot.",
        }
    }

    /// The same choice, described for copying to the export folder.
    pub fn export_description(self) -> &'static str {
        match self {
            DragFiles::Raw => "Just the RAW, to edit later. Shots without a RAW copy their JPG.",
            DragFiles::Jpg => "Just the JPG, ready to share. Shots without a JPG copy their RAW.",
            DragFiles::Both => "Both files of each shot: JPGs to send, RAWs to edit.",
        }
    }

    /// The files of a shot to use: the preferred one, or the other if it's missing.
    pub fn pick<'a>(self, image: Option<&'a PathBuf>, raw: Option<&'a PathBuf>) -> Vec<&'a PathBuf> {
        match self {
            DragFiles::Raw => raw.or(image).into_iter().collect(),
            DragFiles::Jpg => image.or(raw).into_iter().collect(),
            DragFiles::Both => image.into_iter().chain(raw).collect(),
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        let saved = Snapshot {
            export_dir: None,
            recent: Vec::new(),
            show_raw: false,
            tile_width: 190.0,
            drag_files: DragFiles::Raw,
            theme: ThemeId::AppleDark,
            export_on_pick: false,
            export_files: DragFiles::Both,
            sort_by: SortBy::Name,
            sort_descending: false,
        };
        Self::from_snapshot(saved)
    }
}

fn file() -> Option<PathBuf> {
    Some(data_dir()?.join("settings.txt"))
}

impl Settings {
    fn from_snapshot(s: Snapshot) -> Self {
        Self {
            export_dir: s.export_dir.clone(),
            recent: s.recent.clone(),
            show_raw: s.show_raw,
            tile_width: s.tile_width,
            drag_files: s.drag_files,
            theme: s.theme,
            export_on_pick: s.export_on_pick,
            export_files: s.export_files,
            sort_by: s.sort_by,
            sort_descending: s.sort_descending,
            saved: s,
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            export_dir: self.export_dir.clone(),
            recent: self.recent.clone(),
            show_raw: self.show_raw,
            tile_width: self.tile_width,
            drag_files: self.drag_files,
            theme: self.theme,
            export_on_pick: self.export_on_pick,
            export_files: self.export_files,
            sort_by: self.sort_by,
            sort_descending: self.sort_descending,
        }
    }

    pub fn load() -> Self {
        file().map(|f| Self::load_from(&f)).unwrap_or_default()
    }

    fn load_from(path: &std::path::Path) -> Self {
        Self::parse(&std::fs::read_to_string(path).unwrap_or_default())
    }

    fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            match key {
                "export" if !value.is_empty() => s.export_dir = Some(PathBuf::from(value)),
                "recent" if !value.is_empty() => s.recent.push(PathBuf::from(value)),
                "show_raw" => s.show_raw = value == "1",
                "theme" => {
                    if let Some(t) = ThemeId::from_key(value) {
                        s.theme = t;
                    }
                }
                "drag_files" => {
                    if let Some(d) = DragFiles::ALL.into_iter().find(|d| d.key() == value) {
                        s.drag_files = d;
                    }
                }
                "sort_by" => {
                    if let Some(by) = SortBy::ALL.into_iter().find(|b| b.key() == value) {
                        s.sort_by = by;
                    }
                }
                "sort_desc" => s.sort_descending = value == "1",
                "export_on_pick" => s.export_on_pick = value == "1",
                "export_files" => {
                    if let Some(d) = DragFiles::ALL.into_iter().find(|d| d.key() == value) {
                        s.export_files = d;
                    }
                }
                "tile_width" => {
                    if let Ok(v) = value.parse::<f32>() {
                        s.tile_width = v.clamp(120.0, 360.0);
                    }
                }
                _ => {}
            }
        }
        s.recent.truncate(MAX_RECENT);
        s.saved = s.snapshot();
        s
    }

    pub fn save(&mut self) {
        if let Some(path) = file() {
            self.save_to(&path);
        }
    }

    /// Writes only the settings this window changed, on top of what's in the file now.
    /// Another Cullery window (or an older version) may have saved in the meantime;
    /// writing this window's whole stale copy would undo its changes — e.g. put the
    /// theme back to the default.
    fn save_to(&mut self, path: &std::path::Path) {
        let mut merged = Self::load_from(path).snapshot();
        let mine = self.snapshot();
        let base = &self.saved;
        if mine.export_dir != base.export_dir {
            merged.export_dir = mine.export_dir.clone();
        }
        if mine.recent != base.recent {
            merged.recent = mine.recent.clone();
        }
        if mine.show_raw != base.show_raw {
            merged.show_raw = mine.show_raw;
        }
        if mine.tile_width != base.tile_width {
            merged.tile_width = mine.tile_width;
        }
        if mine.drag_files != base.drag_files {
            merged.drag_files = mine.drag_files;
        }
        if mine.theme != base.theme {
            merged.theme = mine.theme;
        }
        if mine.export_on_pick != base.export_on_pick {
            merged.export_on_pick = mine.export_on_pick;
        }
        if mine.export_files != base.export_files {
            merged.export_files = mine.export_files;
        }
        if mine.sort_by != base.sort_by {
            merged.sort_by = mine.sort_by;
        }
        if mine.sort_descending != base.sort_descending {
            merged.sort_descending = mine.sort_descending;
        }

        let mut text = String::new();
        if let Some(dir) = &merged.export_dir {
            text += &format!("export={}\n", dir.display());
        }
        for dir in &merged.recent {
            text += &format!("recent={}\n", dir.display());
        }
        text += &format!("show_raw={}\n", if merged.show_raw { 1 } else { 0 });
        text += &format!("tile_width={}\n", merged.tile_width.round());
        text += &format!("drag_files={}\n", merged.drag_files.key());
        text += &format!("theme={}\n", merged.theme.key());
        text += &format!("export_on_pick={}\n", if merged.export_on_pick { 1 } else { 0 });
        text += &format!("export_files={}\n", merged.export_files.key());
        text += &format!("sort_by={}\n", merged.sort_by.key());
        text += &format!("sort_desc={}\n", if merged.sort_descending { 1 } else { 0 });
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::write(path, text).is_ok() {
            self.saved = mine;
        }
    }

    /// The thumbnail size differs from what was last saved.
    pub fn tile_width_changed(&self) -> bool {
        self.tile_width != self.saved.tile_width
    }

    pub fn add_recent(&mut self, dir: PathBuf) {
        self.recent.retain(|d| d != &dir);
        self.recent.insert(0, dir);
        self.recent.truncate(MAX_RECENT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two windows open at once: one picks a theme, the other later changes something
    /// else. The theme must survive.
    #[test]
    fn saving_keeps_other_windows_changes() {
        let path = std::env::temp_dir().join(format!("cullery-settings-test-{}.txt", std::process::id()));
        std::fs::write(&path, "theme=apple-dark\ntile_width=190\n").unwrap();

        let mut a = Settings::load_from(&path);
        let mut b = Settings::load_from(&path);
        a.theme = ThemeId::Win11Dark;
        a.save_to(&path);
        b.tile_width = 270.0;
        b.save_to(&path);

        let after = Settings::load_from(&path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(after.theme, ThemeId::Win11Dark);
        assert_eq!(after.tile_width, 270.0);
    }

    /// Every setting round-trips through the file.
    #[test]
    fn settings_round_trip() {
        let path = std::env::temp_dir().join(format!("cullery-settings-rt-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut s = Settings::load_from(&path);
        s.export_dir = Some(PathBuf::from(r"D:\Selects"));
        s.add_recent(PathBuf::from(r"F:\DCIM\100CANON"));
        s.show_raw = true;
        s.tile_width = 240.0;
        s.drag_files = DragFiles::Both;
        s.theme = ThemeId::Win10Light;
        s.export_on_pick = true;
        s.export_files = DragFiles::Raw;
        s.sort_by = SortBy::Size;
        s.sort_descending = true;
        s.save_to(&path);
        let back = Settings::load_from(&path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(back.snapshot(), s.snapshot());
    }
}
