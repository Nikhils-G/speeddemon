//! The thread pool behind `par`: workers made by its first call, which wait for
//! the next job (in C0.2 on a CPU with WAITPKG) and meanwhile map the corpus in.

use super::*;

/// f(0), f(1), ... f(n - 1): f(0) on this thread and f(i) on worker i of a pool
/// made by the first call (parts beyond it run on this thread). Between calls the
/// workers wait (`wait`): a call spawns no thread, each worker keeps its heap
/// arena, and its CPU stays out of the deep idle states that take up to 1 ms to
/// wake from. A panic in a job aborts the process (panic = "abort" in Cargo.toml),
/// so no call waits on a worker that died.
pub fn par<T: Send + Sync>(n: usize, f: &(impl Fn(usize) -> T + Sync)) -> Vec<T> {
    // One call at a time: the job slot and the counters below serve one job, so an
    // overlapping call (from another thread, or from a job) would mix two jobs' counts
    // and could return while a worker still runs its job. The program calls par from
    // the main thread only; this makes a misuse stop instead of corrupting memory.
    assert!(!BUSY.swap(true, Acquire), "par: one call at a time");
    let out: Vec<_> = (0..n).map(|_| OnceLock::new()).collect();
    // Part i, if there is one: every worker runs the job with its i.
    let job = |i: usize| _ = out.get(i).map(|r| r.set(f(i)));
    let job: &(dyn Fn(usize) + Sync) = &job;
    // SAFETY: the lifetime is erased so that the static can hold the job. A worker
    // reads JOB only after GENERATION reaches this call's generation, which is
    // published after this store, and holds the read lock until it has run the job;
    // it then counts it in FINISHED. This call returns only once every worker has, so
    // no worker uses the job after it is dropped. The reference left in JOB afterwards
    // is never read: the next call replaces it before it moves GENERATION on.
    *JOB.write().unwrap() = Some(unsafe { transmute(job) });
    let generation = GENERATION.fetch_add(1, Release) + 1;
    let workers = *WORKERS.get_or_init(|| (1..n).map(|i| spawn(move || worker(i))).count());
    (0..n).filter(|&i| i == 0 || i > workers).for_each(job);
    wait(&FINISHED, |f| f >= generation * workers);
    BUSY.store(false, Release);
    out.into_iter().flat_map(OnceLock::into_inner).collect()
}

/// The pool's job, its lifetime erased (see `par`).
static JOB: RwLock<Option<&'static (dyn Fn(usize) + Sync)>> = RwLock::new(None);

/// Set while a call to `par` runs.
static BUSY: AtomicBool = AtomicBool::new(false);

/// The number of jobs so far.
static GENERATION: AtomicUsize = AtomicUsize::new(0);

/// The number of times a worker has finished a job: generation * workers once
/// every worker has finished the current one.
static FINISHED: AtomicUsize = AtomicUsize::new(0);

/// The number of workers, set by the first call to `par`.
static WORKERS: OnceLock<usize> = OnceLock::new();

/// Whether the CPU has WAITPKG (CPUID leaf 7, ECX bit 5), for `wait`.
static WAITPKG: LazyLock<bool> = LazyLock::new(|| __cpuid_count(7, 0).ecx & 1 << 5 != 0);

/// Waits until `done(word)`: in C0.2 (umwait) on a CPU with WAITPKG, woken by a
/// write to the word's cache line or by the kernel's limit (100,000 TSC cycles),
/// so a waiting thread leaves its core to its SMT sibling (in the phase probe,
/// spinning workers with `pause` gained 1.19 ms over spawning threads, umwait
/// 1.29; PERF.md, step 55). Without WAITPKG, it polls.
fn wait(word: &AtomicUsize, done: impl Fn(usize) -> bool) {
    while !done(word.load(Acquire)) && *WAITPKG {
        // SAFETY: the CPU has WAITPKG. umonitor arms the monitor on the word's
        // address, and umwait (deadline u64::MAX, so the kernel's) waits on it
        // unless the word changed meanwhile; neither writes memory.
        unsafe { asm!("umonitor {}", in(reg) word.as_ptr(), options(nostack, preserves_flags)) };
        if !done(word.load(Acquire)) {
            // SAFETY: as above. The register operand 0 asks for C0.2, the deeper of
            // the two states (1 would ask for C0.1).
            unsafe {
                asm!(
                    "umwait {:e}", in(reg) 0, in("edx") u32::MAX, in("eax") u32::MAX,
                    options(nostack)
                )
            };
        }
    }
    while !done(word.load(Acquire)) {
        std::hint::spin_loop();
    }
}

/// The corpus and the number of equal parts it is mapped in by (main's nominal
/// number of slices), once known: workers that wait for a job map its pages in
/// ahead of the scan (`ahead`).
pub static AHEAD: OnceLock<(&'static [u8], usize)> = OnceLock::new();

/// The next chunk for `ahead` to map.
static AHEAD_NEXT: AtomicUsize = AtomicUsize::new(0);

/// Maps in the next 64 KB of the corpus, for a worker that waits for a job, so
/// that the scan finds its pages mapped (it would take the same page faults).
/// Chunk k is the (k / n)-th 64 KB of the (k % n)-th of n equal parts, so every
/// slice gains alike. None once all are mapped. (madvise(MADV_POPULATE_READ)
/// instead held mmap_lock and stalled the main thread's allocations; a page fault
/// takes only its VMA's lock.)
fn ahead() -> Option<()> {
    let &(text, n) = AHEAD.get()?;
    let (k, part) = (AHEAD_NEXT.fetch_add(1, Relaxed), text.len().div_ceil(n));
    let at = k % n * part + (k / n << 16);
    // One read: the kernel maps the 16 pages around it (fault-around).
    text.get(at).map(|&b| std::hint::black_box(b));
    (k / n << 16 < part).then_some(())
}

/// Worker i of `par`'s pool: runs each job with its i.
fn worker(i: usize) {
    for generation in 1.. {
        while GENERATION.load(Acquire) < generation && ahead().is_some() {}
        wait(&GENERATION, |g| g >= generation);
        // The read lock is held until the job is done: `par` writes the next job
        // only once every worker has finished this one.
        JOB.read().unwrap().unwrap()(i);
        FINISHED.fetch_add(1, Release);
    }
}
