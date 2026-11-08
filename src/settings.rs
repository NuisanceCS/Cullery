//! Remembers the export folder, recent folders and view options between runs,
//! in a small text file under %APPDATA%\Cullery.

use std::path::PathBuf;
use std::sync::OnceLock;

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
}

/// Which of a shot's files go along when photos are dragged out of Cullery.
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
}

impl Default for Settings {
    fn default() -> Self {
        Self { export_dir: None, recent: Vec::new(), show_raw: false, tile_width: 190.0, drag_files: DragFiles::Raw }
    }
}

fn file() -> Option<PathBuf> {
    Some(data_dir()?.join("settings.txt"))
}

impl Settings {
    pub fn load() -> Self {
        let mut s = Self::default();
        let Some(text) = file().and_then(|f| std::fs::read_to_string(f).ok()) else {
            return s;
        };
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            match key {
                "export" if !value.is_empty() => s.export_dir = Some(PathBuf::from(value)),
                "recent" if !value.is_empty() => s.recent.push(PathBuf::from(value)),
                "show_raw" => s.show_raw = value == "1",
                "drag_files" => {
                    if let Some(d) = DragFiles::ALL.into_iter().find(|d| d.key() == value) {
                        s.drag_files = d;
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
        s
    }

    pub fn save(&self) {
        let Some(path) = file() else { return };
        let mut text = String::new();
        if let Some(dir) = &self.export_dir {
            text += &format!("export={}\n", dir.display());
        }
        for dir in &self.recent {
            text += &format!("recent={}\n", dir.display());
        }
        text += &format!("show_raw={}\n", if self.show_raw { 1 } else { 0 });
        text += &format!("tile_width={}\n", self.tile_width.round());
        text += &format!("drag_files={}\n", self.drag_files.key());
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, text);
    }

    pub fn add_recent(&mut self, dir: PathBuf) {
        self.recent.retain(|d| d != &dir);
        self.recent.insert(0, dir);
        self.recent.truncate(MAX_RECENT);
    }
}
