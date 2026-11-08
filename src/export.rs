//! Copies photos to the export folder on a background thread, so slow SD cards never
//! freeze the window — and never makes a second copy of the same file, never
//! overwrites one, and never deletes one.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;

struct Job {
    files: Vec<PathBuf>,
    dest: PathBuf,
    batch: Option<u64>,
}

/// A finished copy. `batch` groups the copies of one Quick Export, so the UI can
/// confirm when all of them are done.
pub struct Done {
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
                    let _ = done_tx.send(Done { batch: job.batch, result });
                    ctx.request_repaint();
                }
            })
            .expect("failed to start export thread");
        Self { jobs: Some(job_tx), done: done_rx, thread: Some(thread), pending: 0 }
    }

    pub fn copy(&mut self, files: Vec<PathBuf>, dest: PathBuf, batch: Option<u64>) {
        if let Some(jobs) = &self.jobs {
            if jobs.send(Job { files, dest, batch }).is_ok() {
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

/// A delete running on its own thread, so the window keeps drawing while thousands of
/// files go.
pub struct Deletion {
    total: usize,
    done: Arc<AtomicUsize>,
    result: Receiver<Result<(), String>>,
    thread: Option<JoinHandle<()>>,
}

impl Deletion {
    pub fn start(files: Vec<PathBuf>, permanent: bool, ctx: eframe::egui::Context) -> Self {
        let total = files.len();
        let done = Arc::new(AtomicUsize::new(0));
        let counter = done.clone();
        let (tx, result) = channel();
        let thread = std::thread::Builder::new()
            .name("delete".into())
            .spawn(move || {
                let outcome = delete_files(&files, permanent, |n| {
                    counter.fetch_add(n, Ordering::Relaxed);
                    ctx.request_repaint();
                });
                let _ = tx.send(outcome);
                ctx.request_repaint();
            })
            .expect("failed to start delete thread");
        Self { total, done, result, thread: Some(thread) }
    }

    /// Files deleted so far, and how many there are in all.
    pub fn progress(&self) -> (usize, usize) {
        (self.done.load(Ordering::Relaxed), self.total)
    }

    /// The outcome, once the delete has ended.
    pub fn try_finish(&self) -> Option<Result<(), String>> {
        match self.result.try_recv() {
            Ok(outcome) => Some(outcome),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("The delete stopped unexpectedly.".into())),
        }
    }
}

impl Drop for Deletion {
    /// A delete the user confirmed finishes even if the app is closed meanwhile.
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Deletes photo files: to the Recycle Bin, or permanently when `permanent` (memory
/// cards have no Recycle Bin). Works through them in small groups, calling `progress`
/// with each group's size. Stops at the first failure.
fn delete_files(files: &[PathBuf], permanent: bool, progress: impl Fn(usize)) -> Result<(), String> {
    for group in files.chunks((files.len() / 50).clamp(10, 100)) {
        if permanent {
            for f in group {
                std::fs::remove_file(f).map_err(|e| {
                    format!("Couldn't delete {}: {e}", f.file_name().unwrap_or_default().to_string_lossy())
                })?;
            }
        } else {
            trash::delete_all(group).map_err(|e| format!("Couldn't move the photos to the Recycle Bin: {e}"))?;
        }
        progress(group.len());
    }
    Ok(())
}

/// Whether `path` is somewhere Windows has no Recycle Bin: a removable drive (memory
/// card, USB stick) or a network drive.
#[cfg(windows)]
pub fn has_no_recycle_bin(path: &Path) -> bool {
    use std::path::Component;
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::GetDriveTypeW;
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_REMOTE: u32 = 4;
    let Some(Component::Prefix(prefix)) = path.components().next() else { return false };
    let root = HSTRING::from(format!("{}\\", prefix.as_os_str().to_string_lossy()));
    // SAFETY: reads the type of a drive given its root path.
    matches!(unsafe { GetDriveTypeW(&root) }, DRIVE_REMOVABLE | DRIVE_REMOTE)
}

#[cfg(not(windows))]
pub fn has_no_recycle_bin(_path: &Path) -> bool {
    false
}

/// A file in the export folder counts as a copy of `src` when it has the same size and
/// was last modified at the same time (copying keeps that time). Memory cards can store
/// local time and FAT rounds to 2 seconds, so whole-hour differences (daylight saving,
/// time zones) and small ones still match.
fn is_copy_of(target: &Path, src: &Path) -> bool {
    let (Ok(t), Ok(s)) = (std::fs::metadata(target), std::fs::metadata(src)) else { return false };
    if !t.is_file() || t.len() != s.len() {
        return false;
    }
    let (Ok(t), Ok(s)) = (t.modified(), s.modified()) else { return true };
    let gap = t.duration_since(s).unwrap_or_else(|e| e.duration()).as_secs() % 3600;
    gap <= 2 || gap >= 3598
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The system drive is never removable; a memory card in F: (if present) is.
    #[cfg(windows)]
    #[test]
    fn detects_removable_drives() {
        assert!(!has_no_recycle_bin(Path::new(r"C:\Users")));
        let card = Path::new(r"F:\DCIM");
        if card.exists() {
            println!("F:\\DCIM has no Recycle Bin: {}", has_no_recycle_bin(card));
        }
    }

    /// A big delete runs in the background and counts every file; a file that can't
    /// be deleted is reported.
    #[test]
    fn deletes_in_the_background_with_progress() {
        let base = std::env::temp_dir().join(format!("cullery-delete-bg-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let files: Vec<PathBuf> = (0..250).map(|i| base.join(format!("{i}.jpg"))).collect();
        for f in &files {
            std::fs::write(f, b"x").unwrap();
        }
        let wait = |job: &Deletion| loop {
            if let Some(outcome) = job.try_finish() {
                return outcome;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };

        let job = Deletion::start(files, true, eframe::egui::Context::default());
        assert_eq!(wait(&job), Ok(()));
        assert_eq!(job.progress(), (250, 250));
        drop(job);
        assert_eq!(std::fs::read_dir(&base).unwrap().count(), 0);

        let job = Deletion::start(vec![base.join("missing.jpg")], true, eframe::egui::Context::default());
        assert!(wait(&job).unwrap_err().contains("missing.jpg"));
        drop(job);
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// Same name and size but taken at a different time is a different photo; a
    /// whole-hour difference (daylight saving on a memory card) is still the same one.
    #[test]
    fn a_different_photo_of_the_same_size_is_not_a_copy() {
        use std::time::{Duration, SystemTime};
        let base = std::env::temp_dir().join(format!("cullery-copy-time-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let (src, other) = (base.join("a.jpg"), base.join("b.jpg"));
        std::fs::write(&src, b"same size").unwrap();
        std::fs::write(&other, b"same size").unwrap();
        let when = SystemTime::now() - Duration::from_secs(100_000);
        let stamp = |path: &Path, time: SystemTime| {
            std::fs::OpenOptions::new().write(true).open(path).unwrap().set_modified(time).unwrap();
        };
        stamp(&src, when);
        stamp(&other, when + Duration::from_secs(3600));
        let dst_shifted = is_copy_of(&other, &src);
        stamp(&other, when + Duration::from_secs(4000));
        let different = is_copy_of(&other, &src);
        std::fs::remove_dir_all(&base).unwrap();
        assert!(dst_shifted);
        assert!(!different);
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
