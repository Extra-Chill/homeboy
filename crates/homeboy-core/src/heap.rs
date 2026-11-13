//! Periodically return freed heap memory to the operating system.

#[cfg(all(target_os = "linux", target_env = "gnu"))]
const HEAP_RELEASE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Start one detached heap releaser per process on Linux glibc.
///
/// Repeated calls are harmless. On other platforms this is a no-op.
pub fn spawn_heap_releaser() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        static START: std::sync::Once = std::sync::Once::new();
        START.call_once(|| {
            std::thread::Builder::new()
                .name("homeboy-heap-releaser".into())
                .spawn(|| loop {
                    std::thread::sleep(HEAP_RELEASE_INTERVAL);
                    release_freed_heap_once();
                })
                .expect("spawn heap releaser");
        });
    }
}

/// Return free allocator pages to the operating system.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub(crate) fn release_freed_heap_once() {
    // SAFETY: `malloc_trim` only releases free pages; it takes the allocator's
    // own arena locks and touches no Rust-owned memory.
    unsafe {
        libc::malloc_trim(0);
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
#[allow(dead_code)]
pub(crate) fn release_freed_heap_once() {}

#[cfg(all(test, target_os = "linux", target_env = "gnu"))]
mod tests {
    fn rss_kb() -> usize {
        std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("VmRSS:"))
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap()
    }

    #[test]
    fn heap_releaser_returns_freed_memory() {
        let workers: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(|| {
                    // 64 KiB stays below glibc's mmap threshold. Writing every
                    // byte also faults in every page of the 256 MiB workload.
                    let chunks: Vec<_> = (0..1024).map(|_| vec![1u8; 64 * 1024]).collect();
                    // Keep the arena's top allocated so ordinary free() cannot
                    // automatically trim away the memory under test.
                    let guard = vec![1u8; 64 * 1024];
                    (chunks, guard)
                })
            })
            .collect();
        let allocations: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        let mut guards = Vec::new();
        for (chunks, guard) in allocations {
            guards.push(guard);
            drop(chunks);
        }

        let before = rss_kb();
        super::release_freed_heap_once();
        let after = rss_kb();
        assert!(
            before.saturating_sub(after) >= 128 * 1024,
            "expected at least 128 MiB released: before={before} KiB, after={after} KiB"
        );
        std::hint::black_box(guards);
    }
}
