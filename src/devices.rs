//! Watches for memory cards: any drive with a DCIM folder. Runs on its own thread
//! because touching an absent or sleeping drive can stall for seconds.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct CardFolder {
    /// e.g. `E:\DCIM\100CANON`
    pub path: PathBuf,
    pub jpgs: usize,
    pub raws: usize,
}

pub fn watch(ctx: eframe::egui::Context) -> Arc<Mutex<Vec<CardFolder>>> {
    let found = Arc::new(Mutex::new(Vec::new()));
    let shared = found.clone();
    std::thread::Builder::new()
        .name("devices".into())
        .spawn(move || loop {
            let now = scan();
            let changed = {
                let mut current = shared.lock().unwrap();
                let changed = current.len() != now.len()
                    || current.iter().zip(&now).any(|(a, b): (&CardFolder, &CardFolder)| {
                        a.path != b.path || a.jpgs != b.jpgs || a.raws != b.raws
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

fn scan() -> Vec<CardFolder> {
    let mut found = Vec::new();
    // A: and B: are floppy letters; C: is the system drive.
    for letter in b'D'..=b'Z' {
        let dcim = PathBuf::from(format!("{}:\\DCIM", letter as char));
        let Ok(entries) = std::fs::read_dir(&dcim) else { continue };
        let mut folders: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.path())
            .collect();
        folders.sort();
        for path in folders {
            let (mut jpgs, mut raws) = (0, 0);
            if let Ok(files) = std::fs::read_dir(&path) {
                for f in files.filter_map(|e| e.ok()) {
                    let name = f.file_name().to_string_lossy().to_ascii_lowercase();
                    if name.ends_with(".jpg") || name.ends_with(".jpeg") {
                        jpgs += 1;
                    } else if name.ends_with(".cr3") || name.ends_with(".cr2") {
                        raws += 1;
                    }
                }
            }
            if jpgs + raws > 0 {
                found.push(CardFolder { path, jpgs, raws });
            }
        }
    }
    found
}
