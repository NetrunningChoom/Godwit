// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Keeping memory small (docs/measurements.md). glibc keeps freed memory
//! for reuse, and gives each thread that allocates its own pool ("arena"),
//! so a quiet app can sit on a lot of it; these hand it back.

/// Fewer allocator pools: by default glibc makes up to eight per core, and
/// each keeps its own freed memory. Call once at start, before other threads.
pub fn limit_pools() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        unsafe extern "C" {
            fn mallopt(param: std::ffi::c_int, value: std::ffi::c_int) -> std::ffi::c_int;
        }
        // M_ARENA_MAX (glibc's malloc.h): two pools are plenty for a UI and a
        // few network threads.
        const M_ARENA_MAX: std::ffi::c_int = -8;
        // SAFETY: mallopt only changes the allocator's tuning.
        unsafe { mallopt(M_ARENA_MAX, 2) };
        // M_MMAP_THRESHOLD: glibc maps blocks of 128 KiB or more on their own,
        // but raises that threshold each time it frees one, so after the first
        // decoded video picture every later one stayed in the heap. Fixed at
        // 128 KiB (Yapfast's 9bd993e: in-call heap 39 → 30 MiB), unless the
        // user tuned it through the environment.
        const M_MMAP_THRESHOLD: std::ffi::c_int = -3;
        let tuned = std::env::var_os("MALLOC_MMAP_THRESHOLD_").is_some()
            || std::env::var_os("GLIBC_TUNABLES").is_some_and(|t| t.to_string_lossy().contains("mmap_threshold"));
        if !tuned {
            // SAFETY: as above.
            unsafe { mallopt(M_MMAP_THRESHOLD, 128 * 1024) };
        }
    }
}

/// Gives freed memory back to the system: after big drops, such as leaving a
/// busy room or pictures leaving the cache. On its own thread: trimming a
/// large heap walks all of it, which stuttered the UI.
pub fn settle() {
    let _ = std::thread::Builder::new().name("settle".into()).spawn(settle_now);
}

/// As [`settle`], after `delay`: for memory other threads are still letting
/// go (a call's decoders and audio streams stop on their own threads).
pub fn settle_after(delay: std::time::Duration) {
    let _ = std::thread::Builder::new().name("settle".into()).spawn(move || {
        std::thread::sleep(delay);
        settle_now();
    });
}

/// As [`settle`], on this thread (measurements wait for it).
pub fn settle_now() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        unsafe extern "C" {
            fn malloc_trim(pad: usize) -> std::ffi::c_int;
        }
        // SAFETY: malloc_trim only releases memory the allocator holds
        // unused, and glibc's allocator is safe to call from any thread.
        unsafe { malloc_trim(0) };
    }
}
