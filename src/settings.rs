//! Remembers the export folder, recent folders and view options between runs,
//! in a small text file under %APPDATA%\Culler.

use std::path::PathBuf;

const MAX_RECENT: usize = 8;

pub struct Settings {
    pub export_dir: Option<PathBuf>,
    pub recent: Vec<PathBuf>,
    pub show_raw: bool,
    pub tile_width: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { export_dir: None, recent: Vec::new(), show_raw: false, tile_width: 190.0 }
    }
}

fn file() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("Culler").join("settings.txt"))
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
