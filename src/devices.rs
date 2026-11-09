//! Watches for memory cards: any drive with a DCIM folder. Runs on its own thread
//! because touching an absent or sleeping drive can stall for seconds.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::library::CAMERA_IMAGE_EXTENSIONS;

#[derive(Clone, Debug)]
pub struct CardFolder {
    /// e.g. `E:\DCIM\100CANON`
    pub path: PathBuf,
    pub jpgs: usize,
    pub heics: usize,
    pub raws: usize,
}

pub fn watch(ctx: eframe::egui::Context) -> Arc<Mutex<Vec<CardFolder>>> {
    let found = Arc::new(Mutex::new(Vec::new()));
    let shared = found.clone();
    std::thread::Builder::new()
        .name("devices".into())
        .spawn(move || loop {
            // The app let go of the list: nothing will show it, so stop touching the card.
            if Arc::strong_count(&shared) == 1 {
                return;
            }
            let now = scan();
            let changed = {
                let mut current = shared.lock().unwrap();
                let changed = current.len() != now.len()
                    || current.iter().zip(&now).any(|(a, b): (&CardFolder, &CardFolder)| {
                        a.path != b.path || a.jpgs != b.jpgs || a.heics != b.heics || a.raws != b.raws
                    });
                *current = now;
                changed
            };
            if changed {
                ctx.request_repaint();
            }
            std::thread::sleep(Duration::from_secs(3));
        })
        .expect("failed to start device thread");
    found
}

/// Where a memory card's DCIM folder could be.
#[cfg(not(target_os = "macos"))]
fn dcim_candidates() -> Vec<PathBuf> {
    // A: and B: are floppy letters; C: is the system drive.
    (b'D'..=b'Z').map(|letter| PathBuf::from(format!("{}:\\DCIM", letter as char))).collect()
}

/// Where a memory card's DCIM folder could be.
#[cfg(target_os = "macos")]
fn dcim_candidates() -> Vec<PathBuf> {
    volumes().into_iter().map(|v| v.join("DCIM")).collect()
}

/// The drives plugged in. The startup disk is in /Volumes too, as a link to `/`.
#[cfg(target_os = "macos")]
fn volumes() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir("/Volumes") else { return Vec::new() };
    let mut volumes: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir() && !t.is_symlink()))
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .collect();
    volumes.sort();
    volumes
}

/// Lists the drives that aren't already shown through a DCIM folder of photos.
#[cfg(target_os = "macos")]
fn add_other_drives(found: &mut Vec<CardFolder>) {
    for path in volumes() {
        if !found.iter().any(|card| card.path.starts_with(&path)) {
            found.push(CardFolder { path, jpgs: 0, heics: 0, raws: 0 });
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn add_other_drives(_found: &mut Vec<CardFolder>) {}

fn scan() -> Vec<CardFolder> {
    let mut found = Vec::new();
    for dcim in dcim_candidates() {
        let Ok(entries) = std::fs::read_dir(&dcim) else { continue };
        let mut folders: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.path())
            .collect();
        folders.sort();
        for path in folders {
            let (mut jpgs, mut heics, mut raws) = (0, 0, 0);
            if let Ok(files) = std::fs::read_dir(&path) {
                for f in files.filter_map(|e| e.ok()) {
                    let path = f.path();
                    // macOS leaves "._IMG_0001.JPG" metadata files on cards; they aren't photos.
                    if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("._")) {
                        continue;
                    }
                    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default();
                    if ext.eq_ignore_ascii_case("heic") || ext.eq_ignore_ascii_case("heif") {
                        heics += 1;
                    } else if CAMERA_IMAGE_EXTENSIONS.iter().any(|e| ext.eq_ignore_ascii_case(e)) {
                        jpgs += 1;
                    } else if crate::raw::is_raw_extension(ext) {
                        raws += 1;
                    }
                }
            }
            if jpgs + heics + raws > 0 {
                found.push(CardFolder { path, jpgs, heics, raws });
            }
        }
    }
    add_other_drives(&mut found);
    found
}
