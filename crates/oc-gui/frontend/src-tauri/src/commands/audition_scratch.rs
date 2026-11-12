//! The scratch directory lock coordinates disk writes, eviction, and archive
//! reads across app instances. DSP runs before acquiring the lock.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const BUDGET: u64 = 256 * 1024 * 1024;
const HANDOFF_WINDOW: Duration = Duration::from_secs(15 * 60);

pub(super) struct ScratchRender {
    _lock: File,
    staging: PathBuf,
    output: PathBuf,
}

impl ScratchRender {
    pub(super) fn check_size(bytes: u64) -> Result<(), String> {
        check_size(bytes, BUDGET)
    }

    pub(super) fn reserve(bytes: u64) -> Result<Self, String> {
        Self::reserve_in(&scratch_dir()?, BUDGET, bytes)
    }

    fn reserve_in(dir: &Path, cap: u64, bytes: u64) -> Result<Self, String> {
        check_size(bytes, cap)?;
        crate::private_fs::ensure_dir(dir).map_err(scratch_error)?;
        let lock = directory_lock(dir, false)?;
        make_room(dir, cap, bytes)?;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let output = dir.join(format!(
            "audition-{now}-{}-{}.wav",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        let staging = output.with_extension("wav.part");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)
            .map_err(scratch_error)?;
        Ok(Self {
            _lock: lock,
            staging,
            output,
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.staging
    }

    pub(super) fn publish(self, handoff: bool) -> Result<PathBuf, String> {
        if handoff {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(handoff_path(&self.output))
                .map_err(scratch_error)?;
        }
        let result = self.publish_files();
        if handoff && result.is_err() {
            let _ = fs::remove_file(handoff_path(&self.output));
        }
        result
    }

    fn publish_files(&self) -> Result<PathBuf, String> {
        let staged_grid = crate::render_grid::sidecar_path(&self.staging);
        let output_grid = crate::render_grid::sidecar_path(&self.output);
        if staged_grid.is_file() {
            fs::rename(staged_grid, &output_grid).map_err(scratch_error)?;
        }
        if let Err(error) = fs::rename(&self.staging, &self.output) {
            let _ = fs::remove_file(output_grid);
            return Err(scratch_error(error));
        }
        Ok(self.output.clone())
    }
}

impl Drop for ScratchRender {
    fn drop(&mut self) {
        let _ = remove_render(&self.staging);
    }
}

/// Keep an owned completed render available while the session archive or
/// preview reads it. Explicit outputs are outside the scratch pool.
pub(crate) fn pin_reader(path: &Path) -> Result<Option<File>, String> {
    if !owned_render(path) {
        return Ok(None);
    }
    let Ok(dir) = scratch_dir()?.canonicalize() else {
        return Ok(None);
    };
    let parent = path.parent().and_then(|parent| parent.canonicalize().ok());
    if parent.as_deref() != Some(dir.as_path()) {
        return Ok(None);
    }
    Ok(Some(directory_lock(&dir, true)?))
}

fn handoff_path(path: &Path) -> PathBuf {
    path.with_extension("wav.handoff")
}

/// The archive has finished using the generated source. Only release markers
/// under the application-owned scratch directory.
pub(crate) fn finish_handoff(path: &Path) {
    if !owned_render(path) {
        return;
    }
    let dir = scratch_dir().ok().and_then(|dir| dir.canonicalize().ok());
    let parent = path.parent().and_then(|parent| parent.canonicalize().ok());
    if dir.is_some() && parent == dir {
        let _ = fs::remove_file(handoff_path(path));
    }
}

fn waiting_for_archive(path: &Path, now: SystemTime) -> Result<bool, String> {
    let marker = handoff_path(path);
    let modified = match fs::metadata(&marker).and_then(|meta| meta.modified()) {
        Ok(modified) => modified,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(scratch_error(error)),
    };
    if now.duration_since(modified).unwrap_or_default() < HANDOFF_WINDOW {
        return Ok(true);
    }
    match fs::remove_file(marker) {
        Ok(()) => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(scratch_error(error)),
    }
}

fn scratch_dir() -> Result<PathBuf, String> {
    oc_engine::media_cache::default_cache_dir()
        .and_then(|media| media.parent().map(|root| root.join("audition-scratch")))
        .ok_or_else(|| "Cannot find the audition scratch directory".into())
}

fn check_size(bytes: u64, cap: u64) -> Result<(), String> {
    if bytes > cap {
        return Err(
            "The audition is larger than the scratch budget. Choose an explicit output path."
                .into(),
        );
    }
    Ok(())
}

fn directory_lock(dir: &Path, reading: bool) -> Result<File, String> {
    crate::private_fs::lock(&dir.join(".pool.lock"), reading, !reading).map_err(|error| {
        if error.kind() == io::ErrorKind::WouldBlock {
            "Audition scratch space is busy. Try again or choose an explicit output path.".into()
        } else {
            scratch_error(error)
        }
    })
}

fn make_room(dir: &Path, cap: u64, bytes: u64) -> Result<(), String> {
    let mut used = bytes;
    let now = SystemTime::now();
    let mut candidates = Vec::new();
    for entry in fs::read_dir(dir).map_err(scratch_error)?.flatten() {
        let path = entry.path();
        if !entry.file_type().is_ok_and(|ty| ty.is_file()) {
            continue;
        }
        // Holding the exclusive lock proves no writer still owns staging.
        if let Some(name) = path.file_name().and_then(|name| name.to_str())
            && let Some(output) = name
                .strip_suffix(".part")
                .or_else(|| name.strip_suffix(".part.grid.json"))
            && owned_render(Path::new(output))
        {
            fs::remove_file(&path).map_err(scratch_error)?;
            continue;
        }
        if let Some(wav) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| {
                name.strip_suffix(".grid.json")
                    .or_else(|| name.strip_suffix(".handoff"))
            })
            && owned_render(Path::new(wav))
            && !dir.join(wav).is_file()
        {
            fs::remove_file(&path).map_err(scratch_error)?;
            continue;
        }
        if !owned_render(&path) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let sidecar =
            fs::metadata(crate::render_grid::sidecar_path(&path)).map_or(0, |meta| meta.len());
        let size = meta.len().saturating_add(sidecar);
        used = used.saturating_add(size);
        if !waiting_for_archive(&path, now)? {
            candidates.push((meta.modified().unwrap_or(UNIX_EPOCH), path, size));
        }
    }
    candidates.sort_unstable();
    for (_, path, size) in candidates {
        if used <= cap {
            break;
        }
        if remove_render(&path).is_ok() {
            used = used.saturating_sub(size);
        }
    }
    if used > cap {
        return Err("Audition scratch space is holding renders for the session archive. Try a shorter window or choose an explicit output path.".into());
    }
    Ok(())
}

fn owned_render(path: &Path) -> bool {
    if path.extension().is_none_or(|extension| extension != "wav") {
        return false;
    }
    let Some(stem) = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.strip_prefix("audition-"))
    else {
        return false;
    };
    let mut parts = stem.split('-');
    (0..3).all(|_| {
        parts
            .next()
            .is_some_and(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    }) && parts.next().is_none()
}

fn remove_render(path: &Path) -> io::Result<()> {
    for file in [path.to_path_buf(), crate::render_grid::sidecar_path(path)] {
        if let Err(error) = fs::remove_file(file)
            && error.kind() != io::ErrorKind::NotFound
        {
            return Err(error);
        }
    }
    Ok(())
}

fn scratch_error(error: io::Error) -> String {
    format!("Cannot prepare audition scratch storage: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audition_scratch_recovers_a_stage_after_a_writer_process_dies() {
        use std::io::Read as _;
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        const CHILD_DIR: &str = "OCP_TEST_AUDITION_LOCK_CHILD";
        if let Some(dir) = std::env::var_os(CHILD_DIR).map(PathBuf::from) {
            let scratch = ScratchRender::reserve_in(&dir, 100, 60).unwrap();
            fs::write(scratch.path(), [0; 60]).unwrap();
            fs::write(dir.join("ready"), b"ready").unwrap();
            let _ = io::stdin().read(&mut [0_u8]);
            return;
        }

        let dir = oc_audio::private_fs::test_root().join("audition-scratch-subprocess");
        // A part file left by an earlier run would be found before the child
        // stages its own, and the child's reclaim would then race this test.
        let _ = fs::remove_dir_all(&dir);
        crate::private_fs::ensure_dir(&dir).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "commands::audition_scratch::tests::audition_scratch_recovers_a_stage_after_a_writer_process_dies"])
            .env(CHILD_DIR, &dir)
            // A crash fixture must outlive the writer so the parent can prove
            // that startup reclaims the partial file.
            .env("OCP_TEST_KEEP_SCRATCH", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn().unwrap();
        let start = Instant::now();
        let stage = loop {
            if let Ok(entries) = fs::read_dir(&dir)
                && dir.join("ready").is_file()
                && let Some(stage) = entries
                    .flatten()
                    .map(|entry| entry.path())
                    .find(|path| path.extension().is_some_and(|ext| ext == "part"))
            {
                break stage;
            }
            assert!(
                child.try_wait().unwrap().is_none(),
                "writer exited before staging"
            );
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "writer did not stage"
            );
            std::thread::sleep(Duration::from_millis(2));
        };
        assert!(stage.is_file());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(stage.is_file(), "the crash left a partial file to reclaim");
        let next = ScratchRender::reserve_in(&dir, 100, 60).unwrap();
        assert!(!stage.exists());
        drop(next);
    }

    #[test]
    fn audition_handoff_protects_completed_output_until_release_or_expiry() {
        let dir = scratch_dir().unwrap().join("handoff-budget-fixture");
        let first = ScratchRender::reserve_in(&dir, 100, 60).unwrap();
        fs::write(first.path(), [0; 60]).unwrap();
        let first_path = first.publish(true).unwrap();
        assert!(ScratchRender::reserve_in(&dir, 100, 60).is_err());
        assert!(first_path.is_file());
        assert!(waiting_for_archive(&first_path, SystemTime::now()).unwrap());
        assert!(!waiting_for_archive(&first_path, SystemTime::now() + HANDOFF_WINDOW).unwrap());
        let next = ScratchRender::reserve_in(&dir, 100, 60).unwrap();
        assert!(!first_path.exists());
        drop(next);
    }

    #[test]
    fn implicit_audition_storage_evicts_completed_files_and_guards_active_work() {
        let dir = oc_audio::private_fs::test_root().join("audition-scratch-budget");
        crate::private_fs::ensure_dir(&dir).unwrap();
        let unrelated = dir.join("my-render.wav");
        fs::write(&unrelated, [0; 100]).unwrap();

        let first = ScratchRender::reserve_in(&dir, 100, 60).unwrap();
        assert!(!first.output.exists());
        assert!(ScratchRender::reserve_in(&dir, 100, 60).is_err());
        fs::write(first.path(), [0; 60]).unwrap();
        fs::write(crate::render_grid::sidecar_path(first.path()), b"grid").unwrap();
        let first_path = first.publish(false).unwrap();
        let grid = crate::render_grid::sidecar_path(&first_path);
        assert!(grid.is_file());

        let read = directory_lock(&dir, true).unwrap();
        assert!(ScratchRender::reserve_in(&dir, 100, 60).is_err());
        assert!(first_path.is_file());
        drop(read);

        let orphan = dir.join("audition-1-2-3.wav.part");
        fs::write(&orphan, [0; 60]).unwrap();
        let orphan_grid = dir.join("audition-1-2-3.wav.grid.json");
        fs::write(&orphan_grid, b"grid").unwrap();
        let second = ScratchRender::reserve_in(&dir, 100, 60).unwrap();
        assert!(!first_path.exists());
        assert!(!grid.exists());
        assert!(!orphan.exists());
        assert!(!orphan_grid.exists());
        assert!(unrelated.is_file());
        let abandoned = second.path().to_path_buf();
        drop(second);
        assert!(!abandoned.exists());
        assert!(ScratchRender::reserve_in(&dir, 100, 101).is_err());
    }
}
