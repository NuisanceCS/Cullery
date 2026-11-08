//! Copies photos to the export folder on a background thread, so slow SD cards never
//! freeze the window — and never makes a second copy of the same file, never
//! overwrites one, and never deletes one.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::JoinHandle;

struct Job {
    shot: usize,
    files: Vec<PathBuf>,
    dest: PathBuf,
    batch: Option<u64>,
}

/// A finished copy. `batch` groups the copies of one Quick Export, so the UI can
/// confirm when all of them are done.
pub struct Done {
    #[allow(dead_code)] // reported for completeness; the UI only needs batch + result
    pub shot: usize,
    pub batch: Option<u64>,
    pub result: Result<(), String>,
}

pub struct Exporter {
    jobs: Option<Sender<Job>>,
    done: Receiver<Done>,
    thread: Option<JoinHandle<()>>,
    pending: usize,
}

impl Exporter {
    pub fn new(ctx: eframe::egui::Context) -> Self {
        let (job_tx, job_rx) = channel::<Job>();
        let (done_tx, done_rx) = channel::<Done>();
        let thread = std::thread::Builder::new()
            .name("export".into())
            .spawn(move || {
                for job in job_rx {
                    let result = copy_files(&job.files, &job.dest);
                    let _ = done_tx.send(Done { shot: job.shot, batch: job.batch, result });
                    ctx.request_repaint();
                }
            })
            .expect("failed to start export thread");
        Self { jobs: Some(job_tx), done: done_rx, thread: Some(thread), pending: 0 }
    }

    pub fn copy(&mut self, shot: usize, files: Vec<PathBuf>, dest: PathBuf, batch: Option<u64>) {
        if let Some(jobs) = &self.jobs {
            if jobs.send(Job { shot, files, dest, batch }).is_ok() {
                self.pending += 1;
            }
        }
    }

    pub fn try_recv(&mut self) -> Option<Done> {
        let done = self.done.try_recv().ok()?;
        self.pending = self.pending.saturating_sub(1);
        Some(done)
    }

    /// Number of copy jobs still running.
    pub fn pending(&self) -> usize {
        self.pending
    }
}

impl Drop for Exporter {
    /// Finish any copies still in progress before the app quits.
    fn drop(&mut self) {
        self.jobs.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Where a file's copy in `dest` is or would go: `Ok(Existing)` if `dest` already holds
/// a copy of it (so it's never copied twice), else the first free name — `IMG_0001.JPG`,
/// or `IMG_0001 (2).JPG` when a different file already has that name (it's never
/// overwritten).
enum Target {
    Existing(PathBuf),
    New(PathBuf),
}

fn copy_target(src: &Path, dest: &Path) -> Option<Target> {
    let name = src.file_name()?;
    let stem = src.file_stem()?.to_string_lossy();
    let ext = src.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    for n in 1..=999 {
        let candidate = if n == 1 { dest.join(name) } else { dest.join(format!("{stem} ({n}){ext}")) };
        if is_copy_of(&candidate, src) {
            return Some(Target::Existing(candidate));
        }
        if !candidate.exists() {
            return Some(Target::New(candidate));
        }
    }
    None
}

/// The copy of `src` already in `dest`, if there is one.
pub fn existing_copy(src: &Path, dest: &Path) -> Option<PathBuf> {
    match copy_target(src, dest)? {
        Target::Existing(path) => Some(path),
        Target::New(_) => None,
    }
}

fn copy_files(files: &[PathBuf], dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("Couldn't create {}: {e}", dest.display()))?;
    for src in files {
        let name = src.file_name().unwrap_or_default().to_string_lossy();
        let target = match copy_target(src, dest) {
            Some(Target::Existing(_)) => continue,
            Some(Target::New(path)) => path,
            None => return Err(format!("Too many files named like {name} in the export folder")),
        };
        // Copy under a temporary name first so a pulled card never leaves a half file behind.
        let mut partial = target.clone().into_os_string();
        partial.push(".partial");
        let partial = PathBuf::from(partial);
        std::fs::copy(src, &partial).and_then(|_| std::fs::rename(&partial, &target)).map_err(|e| {
            let _ = std::fs::remove_file(&partial);
            format!("Couldn't copy {name}: {e}")
        })?;
    }
    Ok(())
}

/// Deletes photo files: to the Recycle Bin, or permanently when `permanent` (memory
/// cards have no Recycle Bin). Stops at the first failure.
pub fn delete_files(files: &[PathBuf], permanent: bool) -> Result<(), String> {
    if permanent {
        for f in files {
            std::fs::remove_file(f).map_err(|e| {
                format!("Couldn't delete {}: {e}", f.file_name().unwrap_or_default().to_string_lossy())
            })?;
        }
        Ok(())
    } else {
        trash::delete_all(files).map_err(|e| format!("Couldn't move the photos to the Recycle Bin: {e}"))
    }
}

/// Whether `path` is on a removable drive (memory card, USB stick), where Windows has
/// no Recycle Bin.
#[cfg(windows)]
pub fn on_removable_drive(path: &Path) -> bool {
    use std::path::Component;
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::GetDriveTypeW;
    const DRIVE_REMOVABLE: u32 = 2;
    let Some(Component::Prefix(prefix)) = path.components().next() else { return false };
    let root = HSTRING::from(format!("{}\\", prefix.as_os_str().to_string_lossy()));
    // SAFETY: reads the type of a drive given its root path.
    unsafe { GetDriveTypeW(&root) == DRIVE_REMOVABLE }
}

#[cfg(not(windows))]
pub fn on_removable_drive(_path: &Path) -> bool {
    false
}

/// A file in the export folder counts as a copy of `src` when it has the same size.
fn is_copy_of(target: &Path, src: &Path) -> bool {
    match (std::fs::metadata(target), std::fs::metadata(src)) {
        (Ok(t), Ok(s)) => t.is_file() && t.len() == s.len(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The system drive is never removable; a memory card in F: (if present) is.
    #[cfg(windows)]
    #[test]
    fn detects_removable_drives() {
        assert!(!on_removable_drive(Path::new(r"C:\Users")));
        let card = Path::new(r"F:\DCIM");
        if card.exists() {
            println!("F:\\DCIM removable: {}", on_removable_drive(card));
        }
    }

    /// Copying the same files again makes no new copies; a different file that already
    /// has the name is left alone and the new copy gets "(2)".
    #[test]
    fn never_duplicates_or_overwrites() {
        let base = std::env::temp_dir().join(format!("cullery-copy-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (card, dest) = (base.join("card"), base.join("export"));
        std::fs::create_dir_all(&card).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        let (jpg, raw) = (card.join("IMG_0001.JPG"), card.join("IMG_0001.CR3"));
        std::fs::write(&jpg, b"jpeg bytes").unwrap();
        std::fs::write(&raw, b"raw bytes, longer").unwrap();
        // An edited / unrelated file already using the JPG's name.
        std::fs::write(dest.join("IMG_0001.JPG"), b"someone else's edit").unwrap();

        let files = vec![jpg.clone(), raw.clone()];
        for _ in 0..3 {
            copy_files(&files, &dest).unwrap();
        }
        let mut names: Vec<String> =
            std::fs::read_dir(&dest).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        let edit_kept = std::fs::read(dest.join("IMG_0001.JPG")).unwrap() == b"someone else's edit";
        let found = existing_copy(&jpg, &dest);
        std::fs::remove_dir_all(&base).unwrap();

        assert_eq!(names, ["IMG_0001 (2).JPG", "IMG_0001.CR3", "IMG_0001.JPG"]);
        assert!(edit_kept, "the existing file isn't overwritten");
        assert_eq!(found, Some(dest.join("IMG_0001 (2).JPG")));
    }
}
