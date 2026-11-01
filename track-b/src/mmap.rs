//! Read-only mmap and other libc calls, declared by hand so the crate has no dependencies.

use std::io::Error;
use std::os::fd::AsRawFd;

// The Linux x86-64 values of the libc constants used below.
const PR_SET_THP_DISABLE: i32 = 41;
const PROT_READ: i32 = 1;
const MAP_PRIVATE: i32 = 0x02;
const MAP_FIXED: i32 = 0x10;
const MAP_ANONYMOUS: i32 = 0x20;
const MADV_DONTNEED: i32 = 4;

unsafe extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut u8;
    fn prctl(option: i32, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> i32;
    fn madvise(addr: *mut u8, len: usize, advice: i32) -> i32;
}

/// Turns off transparent huge pages for this process. With THP set to "always",
/// touching one byte of a sparse, lazily zeroed table can fault in a whole 2 MB
/// page, so the memory peak would depend on how fragmented the host is that run.
pub fn no_huge_pages() {
    // SAFETY: a plain syscall wrapper; failure only means THP stays on.
    unsafe { prctl(PR_SET_THP_DISABLE, 1, 0, 0, 0) };
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

/// Drops this process's page-table entries for the pages that `slice` touches, a
/// part of a read-only file mapping that is done with. At exit the kernel would
/// otherwise unmap the 62,000 pages of the input one by one, on one core; this
/// does the same work in each scanning thread, in parallel. Reading the bytes
/// again is still correct: the page faults back in from the page cache.
///
/// # Safety
///
/// Only for slices of a mapping made by `map`: on heap memory it would zero live data.
pub unsafe fn release(slice: &[u8]) {
    // madvise takes a page-aligned start; the end is rounded up to whole pages.
    // The pages at the ends may also hold the neighbouring slices' bytes.
    let start = slice.as_ptr() as usize / 4096 * 4096;
    let len = slice.as_ptr() as usize + slice.len() - start;
    // SAFETY: whole pages of a private, read-only mapping that is never written
    // (the file, then zero pages: the caller's contract); dropping them loses
    // nothing.
    unsafe { madvise(start as *mut u8, len, MADV_DONTNEED) };
}
