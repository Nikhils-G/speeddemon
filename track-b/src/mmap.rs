//! Read-only mmap and other libc calls, declared by hand so the crate has no dependencies.

use std::{io::Error, os::fd::AsRawFd};

// The Linux x86-64 values of the libc constants used below.
const PR_SET_THP_DISABLE: i32 = 41;
const PROT_READ: i32 = 1;
const MAP_PRIVATE: i32 = 0x02;
const MAP_FIXED: i32 = 0x10;
const MAP_ANONYMOUS: i32 = 0x20;
const MADV_DONTNEED: i32 = 4;
const M_TRIM_THRESHOLD: i32 = -1;
const M_MMAP_THRESHOLD: i32 = -3;
/// x86-64's base page size.
const PAGE: usize = 4096;

// SAFETY: glibc's x86-64 prototypes. prctl is variadic in C (int prctl(int, ...)); its
// integer arguments go in the same registers either way, and %al, which a variadic
// callee reads only to decide whether to save vector registers, may hold any value.
unsafe extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut u8;
    fn prctl(option: i32, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> i32;
    fn madvise(addr: *mut u8, len: usize, advice: i32) -> i32;
    fn mallopt(param: i32, value: i32) -> i32;
}

/// Sets how this process gets memory:
/// - No transparent huge pages. With THP set to "always", touching one byte of a
///   sparse, lazily zeroed table can fault in a whole 2 MB page, so the memory peak
///   would depend on how fragmented the host is that run.
/// - Allocations of up to 1 MB come from malloc's heaps, one per thread, instead
///   of an mmap each, and the threshold stays fixed: by default it rises with the
///   largest block freed so far, so where each step's tables come from (fresh
///   pages or reused ones) would depend on what the steps before it freed. The
///   setup's parallel steps then fault in fewer fresh pages, each in its thread's
///   own mapping. Larger blocks, such as the scan's first-id logs, stay mmaps,
///   whose pages cost nothing until written: from a heap, calloc would clear them
///   (PERF.md, step 67).
/// - Freed memory stays in the heaps: a trim (an sbrk shrink) flushes the TLB of
///   every CPU the threads ran on, and each step frees tables that the next ones
///   reuse (PERF.md, step 82).
pub fn set_up_memory() {
    // SAFETY: a plain syscall wrapper; failure only means THP stays on.
    unsafe { prctl(PR_SET_THP_DISABLE, 1, 0, 0, 0) };
    // SAFETY: a libc call that touches no memory of ours; failure keeps the default.
    unsafe { mallopt(M_MMAP_THRESHOLD, 1 << 20) };
    // SAFETY: as above.
    unsafe { mallopt(M_TRIM_THRESHOLD, 1 << 30) };
}

/// Zero bytes mapped after the end of every file: fixed-size loads (64-byte
/// chunks, 16-byte keys) at any offset inside the file then stay in bounds, and
/// zero is a separator.
pub const PAD: usize = 256;

/// Maps the file at `path` read-only, followed by PAD zero bytes, for the rest of
/// the process. It is never unmapped: the process exits right after using it.
pub fn map(path: &str) -> &'static [u8] {
    let file = std::fs::File::open(path).expect(path);
    let len = file.metadata().expect(path).len() as usize;
    let check = |p: *mut u8| assert!(p as isize != -1, "mmap {path}: {}", Error::last_os_error());
    // Reserve the whole range as zero pages first, then map the file over its start.
    let (anonymous, fixed) = (MAP_PRIVATE | MAP_ANONYMOUS, MAP_PRIVATE | MAP_FIXED);
    // SAFETY: a fresh anonymous read-only mapping; nothing else refers to it.
    let base = unsafe { mmap(std::ptr::null_mut(), len + PAD, PROT_READ, anonymous, -1, 0) };
    check(base);
    if len > 0 {
        // SAFETY: MAP_FIXED only replaces pages of the reservation made above. The
        // dataset is mounted read-only, so the bytes cannot change under us.
        check(unsafe { mmap(base, len, PROT_READ, fixed, file.as_raw_fd(), 0) });
    }
    // SAFETY: `len + PAD` bytes are mapped (the file, then zeros: the tail of the
    // file's last page reads as zero) and stay mapped until exit.
    unsafe { std::slice::from_raw_parts(base, len + PAD) }
}

/// Gives the whole pages inside `slice` back to the kernel (MADV_DONTNEED), so
/// that the exit, on one core, has fewer to unmap and free. A file mapping's pages
/// would fault back in from the page cache if read again; anonymous memory would
/// read as zero.
///
/// # Safety
///
/// Nothing reads or writes `slice` again: the process is about to exit.
pub unsafe fn release<T>(slice: &[T]) {
    let start = (slice.as_ptr() as usize).next_multiple_of(PAGE);
    let end = (slice.as_ptr() as usize + size_of_val(slice)) / PAGE * PAGE;
    // SAFETY: whole pages inside the slice (none if end <= start), never used
    // again (the caller's contract).
    unsafe { madvise(start as *mut u8, end.saturating_sub(start), MADV_DONTNEED) };
}
