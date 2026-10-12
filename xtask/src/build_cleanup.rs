//! Reclaim disposable native debug intermediates without invalidating finished artifacts.
use std::fs::{self, File, TryLockError};
use std::io;
use std::path::Path;

#[derive(Debug, Default)]
pub struct Summary {
    pub object_files: u64,
    pub incremental: bool,
}

fn directory(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(format!("refusing non-directory or symlink: {}", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn entries(path: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    // Snapshot before unlinking: mutating a large APFS directory during readdir can skip entries.
    fs::read_dir(path).map_err(|e| format!("{}: {e}", path.display()))?.map(|e| e.map(|e| e.path()).map_err(|e| format!("{}: {e}", path.display()))).collect()
}

fn remove_cache(path: &Path, depth: usize) -> Result<(), String> {
    if depth > 32 {
        return Err(format!("incremental cache nesting exceeds 32 at {}", path.display()));
    }
    let meta = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.is_dir() && !meta.file_type().is_symlink() {
        for child in entries(path)? {
            remove_cache(&child, depth.saturating_add(1))?;
        }
        fs::remove_dir(path).map_err(|e| format!("{}: {e}", path.display()))
    } else {
        fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// The caller supplies a workspace-local target directory. Never follow a target/debug symlink.
/// Keep Cargo's lock inode and hold its exclusive lock throughout inspection and deletion.
pub fn clean(target: &Path, dry_run: bool) -> Result<Summary, String> {
    if !directory(target)? {
        return Ok(Summary::default());
    }
    let debug = target.join("debug");
    if !directory(&debug)? {
        return Ok(Summary::default());
    }
    let lock_path = debug.join(".cargo-lock");
    let meta = fs::symlink_metadata(&lock_path).map_err(|e| format!("{}: {e}; refusing unlocked cleanup", lock_path.display()))?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err("refusing a non-regular Cargo lock file".into());
    }
    let lock = File::options().read(true).write(true).open(&lock_path).map_err(|e| format!("{}: {e}", lock_path.display()))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Err("Cargo is using target/debug; cleanup skipped".into()),
        Err(TryLockError::Error(e)) => return Err(format!("cannot lock target/debug: {e}")),
    }
    // Validate every candidate directory before removing anything. Linked directories are not ours.
    let incremental = debug.join("incremental");
    let has_incremental = directory(&incremental)?;
    let dirs = [debug.join("deps"), debug.join("examples")];
    let present: Vec<bool> = dirs.iter().map(|p| directory(p)).collect::<Result<_, _>>()?;
    let mut summary = Summary { incremental: has_incremental, ..Summary::default() };
    for (dir, exists) in dirs.iter().zip(present) {
        if !exists {
            continue;
        }
        for path in entries(dir)? {
            let kind = fs::symlink_metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?.file_type();
            if kind.is_file() && path.file_name().is_some_and(|n| n.to_string_lossy().ends_with(".rcgu.o")) {
                summary.object_files = summary.object_files.saturating_add(1);
                if !dry_run {
                    fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                }
            }
        }
    }
    // Remove both hard-linked codegen objects above and the cache that retains their other links.
    if has_incremental && !dry_run {
        remove_cache(&incremental, 0)?;
    }
    Ok(summary)
}

pub fn report(summary: &Summary, dry_run: bool) {
    if summary.object_files > 0 || summary.incremental {
        eprintln!(
            "build cleanup: {} {} codegen objects{}; finished binaries, libraries and fingerprints retained",
            if dry_run { "would remove" } else { "removed" },
            summary.object_files,
            if summary.incremental { " and the incremental cache" } else { "" }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("photocraft-build-cleanup-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            fs::create_dir_all(dir.join("debug/deps")).expect("fixture");
            fs::create_dir_all(dir.join("debug/examples")).expect("fixture");
            fs::create_dir_all(dir.join("debug/incremental/crate/session")).expect("fixture");
            for name in [
                ".cargo-lock",
                "photocraft-cli",
                "deps/libengine.rlib",
                "deps/libengine.rmeta",
                "deps/engine.d",
                "deps/test-binary",
                "deps/keep.o",
                "deps/engine.abc.rcgu.o",
                "examples/example.abc.rcgu.o",
                "examples/snapshot",
                "incremental/crate/session/query-cache.bin",
            ] {
                fs::write(dir.join("debug").join(name), b"fixture").expect("fixture");
            }
            Self(dir)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn cleanup_keeps_outputs_and_is_repeatable() {
        let f = Fixture::new();
        let s = clean(&f.0, false).expect("cleanup");
        assert_eq!(s.object_files, 2);
        assert!(s.incremental);
        assert!(!f.0.join("debug/incremental").exists());
        for name in [
            ".cargo-lock",
            "photocraft-cli",
            "deps/libengine.rlib",
            "deps/libengine.rmeta",
            "deps/engine.d",
            "deps/test-binary",
            "deps/keep.o",
            "examples/snapshot",
        ] {
            assert_eq!(fs::read(f.0.join("debug").join(name)).expect("retained"), b"fixture");
        }
        assert_eq!(clean(&f.0, false).expect("repeat").object_files, 0);
    }

    #[test]
    fn dry_run_does_not_delete() {
        let f = Fixture::new();
        assert_eq!(clean(&f.0, true).expect("dry run").object_files, 2);
        assert!(f.0.join("debug/deps/engine.abc.rcgu.o").exists());
        assert!(f.0.join("debug/incremental/crate/session/query-cache.bin").exists());
    }

    #[test]
    fn busy_cargo_lock_prevents_all_deletion() {
        let f = Fixture::new();
        let lock = File::options().read(true).write(true).open(f.0.join("debug/.cargo-lock")).expect("lock");
        lock.lock().expect("lock acquired");
        assert!(clean(&f.0, false).expect_err("busy").contains("Cargo is using"));
        assert!(f.0.join("debug/deps/engine.abc.rcgu.o").exists());
        assert!(f.0.join("debug/incremental/crate/session/query-cache.bin").exists());
    }

    #[test]
    fn missing_lock_refuses_cleanup() {
        let f = Fixture::new();
        fs::remove_file(f.0.join("debug/.cargo-lock")).expect("remove lock");
        assert!(clean(&f.0, false).is_err());
        assert!(f.0.join("debug/incremental").exists());
    }

    #[test]
    fn cache_and_codegen_hardlinks_are_both_removed() {
        let f = Fixture::new();
        fs::hard_link(f.0.join("debug/deps/engine.abc.rcgu.o"), f.0.join("debug/incremental/crate/session/engine.o")).expect("hard link");
        clean(&f.0, false).expect("cleanup");
        assert!(!f.0.join("debug/deps/engine.abc.rcgu.o").exists());
        assert!(!f.0.join("debug/incremental/crate/session/engine.o").exists());
    }

    #[test]
    fn large_directories_are_completely_removed() {
        let f = Fixture::new();
        for n in 0..5000 {
            fs::write(f.0.join("debug/deps").join(format!("engine.{n}.rcgu.o")), b"object").expect("fixture");
            fs::write(f.0.join("debug/incremental/crate/session").join(format!("object-{n}.o")), b"object").expect("fixture");
        }
        assert_eq!(clean(&f.0, false).expect("cleanup").object_files, 5002);
        assert!(!f.0.join("debug/incremental").exists());
        assert!(!entries(&f.0.join("debug/deps")).expect("entries").iter().any(|p| p.to_string_lossy().ends_with(".rcgu.o")));
    }

    #[cfg(unix)]
    #[test]
    fn target_and_lock_symlinks_are_refused() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new();
        let outside = Fixture::new();
        let linked_target = f.0.join("linked-target");
        symlink(&outside.0, &linked_target).expect("link");
        assert!(clean(&linked_target, false).is_err());
        fs::remove_file(f.0.join("debug/.cargo-lock")).expect("replace lock");
        symlink(outside.0.join("debug/.cargo-lock"), f.0.join("debug/.cargo-lock")).expect("link lock");
        assert!(clean(&f.0, false).is_err());
        assert!(outside.0.join("debug/deps/engine.abc.rcgu.o").exists());
    }

    #[cfg(unix)]
    #[test]
    fn nested_cache_symlinks_do_not_delete_their_targets() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new();
        let outside = Fixture::new();
        symlink(&outside.0, f.0.join("debug/incremental/crate/session/linked")).expect("link");
        clean(&f.0, false).expect("cleanup");
        assert!(outside.0.join("debug/deps/engine.abc.rcgu.o").exists());
    }

    #[cfg(unix)]
    #[test]
    fn linked_directories_and_files_are_not_followed() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new();
        let outside = Fixture::new();
        symlink(outside.0.join("debug/deps/engine.abc.rcgu.o"), f.0.join("debug/deps/linked.rcgu.o")).expect("link");
        clean(&f.0, false).expect("cleanup");
        assert!(outside.0.join("debug/deps/engine.abc.rcgu.o").exists());
        assert!(f.0.join("debug/deps/linked.rcgu.o").symlink_metadata().is_ok());
        fs::remove_dir_all(f.0.join("debug/deps")).expect("replace deps");
        symlink(outside.0.join("debug/deps"), f.0.join("debug/deps")).expect("link dir");
        assert!(clean(&f.0, false).is_err());
        assert!(outside.0.join("debug/deps/engine.abc.rcgu.o").exists());
    }
}
