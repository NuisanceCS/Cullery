//! Copies picked shots (JPG + RAW) to the export folder on a background thread,
//! so slow SD cards never freeze the window.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::JoinHandle;

enum Job {
    Copy { shot: usize, files: Vec<PathBuf>, dest: PathBuf, batch: Option<u64> },
    Remove { shot: usize, files: Vec<PathBuf>, dest: PathBuf },
}

pub enum Done {
    /// `batch` groups the copies of one Quick Export, so the UI can confirm when all finish.
    Copied { shot: usize, batch: Option<u64>, result: Result<(), String> },
    Removed { shot: usize, result: Result<(), String> },
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
                    let done = match job {
                        Job::Copy { shot, files, dest, batch } => Done::Copied {
                            shot,
                            batch,
                            result: copy_files(&files, &dest),
                        },
                        Job::Remove { shot, files, dest } => Done::Removed {
                            shot,
                            result: remove_copies(&files, &dest),
                        },
                    };
                    let _ = done_tx.send(done);
                    ctx.request_repaint();
                }
            })
            .expect("failed to start export thread");
        Self { jobs: Some(job_tx), done: done_rx, thread: Some(thread), pending: 0 }
    }

    pub fn copy(&mut self, shot: usize, files: Vec<PathBuf>, dest: PathBuf, batch: Option<u64>) {
        self.send(Job::Copy { shot, files, dest, batch });
    }

    pub fn remove(&mut self, shot: usize, files: Vec<PathBuf>, dest: PathBuf) {
        self.send(Job::Remove { shot, files, dest });
    }

    fn send(&mut self, job: Job) {
        if let Some(jobs) = &self.jobs {
            if jobs.send(job).is_ok() {
                self.pending += 1;
            }
        }
    }

    pub fn try_recv(&mut self) -> Option<Done> {
        let done = self.done.try_recv().ok()?;
        self.pending = self.pending.saturating_sub(1);
        Some(done)
    }

    /// Number of copy/remove jobs still running.
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

fn copy_files(files: &[PathBuf], dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("Couldn't create {}: {e}", dest.display()))?;
    for src in files {
        let Some(name) = src.file_name() else { continue };
        let target = dest.join(name);
        if is_copy_of(&target, src) {
            continue;
        }
        // Copy under a temporary name first so a pulled card never leaves a half file behind.
        let mut partial = target.clone().into_os_string();
        partial.push(".partial");
        let partial = PathBuf::from(partial);
        std::fs::copy(src, &partial)
            .and_then(|_| std::fs::rename(&partial, &target))
            .map_err(|e| {
                let _ = std::fs::remove_file(&partial);
                format!("Couldn't copy {}: {e}", name.to_string_lossy())
            })?;
    }
    Ok(())
}

/// Un-picking moves the copies this app made to the Recycle Bin. Files in the export
/// folder that don't match the original (e.g. edited since) are left alone.
fn remove_copies(files: &[PathBuf], dest: &Path) -> Result<(), String> {
    for src in files {
        let Some(name) = src.file_name() else { continue };
        let target = dest.join(name);
        if is_copy_of(&target, src) {
            trash::delete(&target)
                .map_err(|e| format!("Couldn't move {} to the Recycle Bin: {e}", name.to_string_lossy()))?;
        }
    }
    Ok(())
}

fn is_copy_of(target: &Path, src: &Path) -> bool {
    match (std::fs::metadata(target), std::fs::metadata(src)) {
        (Ok(t), Ok(s)) => t.is_file() && t.len() == s.len(),
        _ => false,
    }
}
