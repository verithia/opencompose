//! Dependency-free instrumentation for tests of real-time runtime operations.
//! A test binary installs [`CountingAllocator`] as its global allocator, then
//! uses [`operations`] to measure heap activity on the calling thread.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static ENABLED: Cell<bool> = const { Cell::new(false) };
    static OPERATIONS: Cell<usize> = const { Cell::new(0) };
}

/// System allocator with optional, thread-local operation counting.
///
/// Allocation, zeroed allocation, reallocation and deallocation each count as
/// one operation, including a failed allocation attempt. Instrumentation never
/// allocates, retains pointers or changes an allocation's layout or lifetime.
/// Activity outside an [`operations`] scope passes through without counting.
/// Install one instance with Rust's `global_allocator` attribute in each test
/// binary that needs to detect runtime heap activity.
pub struct CountingAllocator;

fn record() {
    if ENABLED.try_with(Cell::get).unwrap_or(false) {
        let _ = OPERATIONS.try_with(|count| count.set(count.get().wrapping_add(1)));
    }
}

// SAFETY: Every operation forwards its unchanged pointer/layout contract to
// System. The thread-local counters do not allocate or retain pointers.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record();
        // SAFETY: Forward the caller's valid allocation layout to System.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        record();
        // SAFETY: Forward the caller's allocation and original layout unchanged.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record();
        // SAFETY: Forward the caller's valid allocation layout to System.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record();
        // SAFETY: Forward the caller's valid realloc arguments unchanged.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

/// Count allocator operations performed by `action` on the calling thread.
///
/// An outer scope includes the operations of its nested scopes, while each
/// nested scope returns its own count. A panic restores the prior measurement
/// state before unwinding continues. Other threads are measured only by scopes
/// they start themselves. Dropping a preexisting allocation inside `action`
/// counts, so the result detects runtime deallocation as well as allocation.
pub fn operations(action: impl FnOnce()) -> usize {
    struct Guard {
        was_enabled: bool,
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            ENABLED.set(self.was_enabled);
        }
    }

    let before = OPERATIONS.get();
    let guard = Guard {
        was_enabled: ENABLED.replace(true),
    };
    action();
    drop(guard);
    OPERATIONS.get().wrapping_sub(before)
}

/// A persistent build-artifact directory under `target/fixtures`.
///
/// Unlike [`temp_dir`], this directory survives the test run; use it only for
/// artifacts that are expensive to rebuild and safe to reuse.
pub fn build_dir(name: &str) -> std::path::PathBuf {
    let root = temp_root()
        .parent()
        .map(|parent| parent.join("fixtures"))
        .unwrap_or_else(|| temp_root().join("fixtures"))
        .join(name);
    std::fs::create_dir_all(&root).expect("create build fixture directory");
    root
}

static ACTIVE_TEMP_THREADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static TEMP_ROOT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct ThreadTempPaths(std::cell::RefCell<Vec<std::path::PathBuf>>);

impl Drop for ThreadTempPaths {
    fn drop(&mut self) {
        let paths = self.0.get_mut();
        for path in paths.iter() {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_dir_all(path);
        }
        let _guard = TEMP_ROOT_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if ACTIVE_TEMP_THREADS.fetch_sub(1, std::sync::atomic::Ordering::AcqRel) == 1
            && let Some(root) = paths.first().and_then(|path| path.parent())
        {
            let _ = std::fs::remove_dir(root);
            if let Some(parent) = root.parent() {
                let _ = std::fs::remove_dir(parent);
            }
        }
    }
}

thread_local! {
    static THREAD_TEMP: ThreadTempPaths = const { ThreadTempPaths(std::cell::RefCell::new(Vec::new())) };
    static TEMP_THREAD_REGISTERED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn temp_root() -> std::path::PathBuf {
    if let Some(root) = std::env::var_os("CARGO_TARGET_DIR") {
        return std::path::PathBuf::from(root).join("test-tmp");
    }
    let mut dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    loop {
        if dir.join("Cargo.toml").is_file() && dir.join("target").is_dir() {
            return dir.join("target").join("test-tmp");
        }
        if !dir.pop() {
            break;
        }
    }
    std::env::temp_dir().join("opencompose-tests")
}

fn temp_location(prefix: &str, name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    TEMP_THREAD_REGISTERED.with(|registered| {
        if !registered.replace(true) {
            let _guard = TEMP_ROOT_LOCK
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            ACTIVE_TEMP_THREADS.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }
    });
    let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = temp_root().join(std::process::id().to_string());
    std::fs::create_dir_all(&root).expect("create temporary test directory");
    root.join(format!(
        "{prefix}-{name}-{:?}-{unique}",
        std::thread::current().id()
    ))
}

fn register_temp_path(path: &std::path::Path) {
    THREAD_TEMP.with(|paths| {
        paths.0.borrow_mut().push(path.to_path_buf());
    });
}

/// Create an empty, process-, thread- and call-scoped temporary directory.
///
/// The directory lives under `target/test-tmp` (or `CARGO_TARGET_DIR`) and is
/// removed when the test thread exits. A process killed before the thread
/// unwinds leaves its directory behind for the next test run's cleanup; tests
/// must not rely on the path outliving the thread.
pub fn temp_dir(prefix: &str, name: &str) -> std::path::PathBuf {
    let dir = temp_location(prefix, name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temporary test directory");
    register_temp_path(&dir);
    dir
}

/// A temporary path under the test root, without creating it.
///
/// The path is removed when the test thread exits if a test creates a file or
/// directory there.
pub fn temp_path(prefix: &str, name: &str) -> std::path::PathBuf {
    let path = temp_location(prefix, name);
    register_temp_path(&path);
    path
}

/// Write `bytes` to a temporary file under the test root.
pub fn temp_file(prefix: &str, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = temp_location(prefix, name);
    std::fs::write(&path, bytes).expect("write temporary test file");
    register_temp_path(&path);
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[global_allocator]
    static ALLOCATOR: CountingAllocator = CountingAllocator;

    fn allocation_pair() {
        let layout = Layout::new::<u64>();
        // SAFETY: The nonzero layout is valid. Its allocation is checked before
        // use and released exactly once with the original layout.
        unsafe {
            let pointer = std::alloc::alloc(layout);
            assert!(!pointer.is_null());
            std::hint::black_box(pointer).write(1);
            std::alloc::dealloc(pointer, layout);
        }
    }

    #[test]
    fn counts_all_allocator_entry_points() {
        let count = operations(|| {
            let layout = Layout::from_size_align(16, 8).unwrap();
            let grown_layout = Layout::from_size_align(32, 8).unwrap();
            // SAFETY: Both layouts are valid and nonzero. Allocations are
            // checked before access and released with their current layouts.
            unsafe {
                let pointer = std::alloc::alloc(layout);
                assert!(!pointer.is_null());
                let grown = std::alloc::realloc(pointer, layout, 32);
                if grown.is_null() {
                    std::alloc::dealloc(pointer, layout);
                    panic!("test allocation failed");
                }
                std::hint::black_box(grown).write(1);
                std::alloc::dealloc(grown, grown_layout);
                let zeroed = std::alloc::alloc_zeroed(layout);
                assert!(!zeroed.is_null());
                assert_eq!(std::hint::black_box(zeroed).read(), 0);
                std::alloc::dealloc(zeroed, layout);
            }
        });
        assert_eq!(count, 5);
    }

    #[test]
    fn nested_scopes_preserve_outer_measurement() {
        let mut inner = 0;
        let outer = operations(|| {
            allocation_pair();
            inner = operations(allocation_pair);
            allocation_pair();
        });
        assert_eq!(inner, 2);
        assert_eq!(outer, 6);
        assert_eq!(operations(|| {}), 0);
    }

    #[test]
    fn panic_restores_inactive_and_nested_measurement_states() {
        let failed = std::panic::catch_unwind(|| operations(|| panic!("scope panic")));
        assert!(failed.is_err());
        assert!(!ENABLED.get());
        assert_eq!(operations(allocation_pair), 2);
        operations(|| {
            let failed = std::panic::catch_unwind(|| operations(|| panic!("nested panic")));
            assert!(failed.is_err());
            assert!(ENABLED.get());
            assert_eq!(operations(allocation_pair), 2);
        });
        assert!(!ENABLED.get());
    }

    #[test]
    fn scope_does_not_count_other_threads() {
        use std::sync::atomic::{AtomicU8, Ordering};

        let state = AtomicU8::new(0);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                state.store(1, Ordering::Release);
                while state.load(Ordering::Acquire) != 2 {
                    std::hint::spin_loop();
                }
                let count = operations(allocation_pair);
                state.store(3, Ordering::Release);
                count
            });
            let caller_count = operations(|| {
                while state.load(Ordering::Acquire) != 1 {
                    std::hint::spin_loop();
                }
                state.store(2, Ordering::Release);
                while state.load(Ordering::Acquire) != 3 {
                    std::hint::spin_loop();
                }
            });
            assert_eq!(caller_count, 0);
            assert_eq!(worker.join().unwrap(), 2);
        });
    }

    #[test]
    fn dropping_preexisting_storage_counts_and_disabled_scopes_stay_idle() {
        let allocation = Box::new(std::hint::black_box(7u64));
        assert_eq!(operations(|| drop(std::hint::black_box(allocation))), 1);
        let before = OPERATIONS.get();
        allocation_pair();
        assert_eq!(OPERATIONS.get(), before);
    }

    #[test]
    fn temp_dir_is_empty_and_call_unique() {
        let first = temp_dir("oc-test-support", "case");
        let second = temp_dir("oc-test-support", "case");
        assert_ne!(first, second);
        for dir in [&first, &second] {
            assert!(dir.is_dir());
            assert_eq!(std::fs::read_dir(dir).unwrap().count(), 0);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn concurrent_temp_dirs_are_unique_and_removed_with_their_thread() {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|index| {
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    let dir = temp_dir("oc-test-concurrent", &format!("thread-{index}"));
                    std::fs::write(dir.join("payload"), b"x").unwrap();
                    dir
                })
            })
            .collect();
        let paths: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        let unique: std::collections::HashSet<_> = paths.iter().collect();
        assert_eq!(unique.len(), paths.len());
        assert!(
            paths.iter().all(|path| !path.exists()),
            "a test thread's temporary directory must not outlive it"
        );
    }
}
