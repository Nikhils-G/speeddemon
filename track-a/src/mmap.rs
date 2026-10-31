//! mmap, madvise and prctl through libc, declared by hand so the crate has no
//! dependencies. Mappings are never unmapped: the process exits right after
//! using them.

use std::fs::File;
use std::io::Error;
use std::os::fd::AsRawFd;
use std::os::raw::{c_int, c_long, c_void};
use std::ptr::null_mut;

unsafe extern "C" {
    fn mmap(
        at: *mut c_void,
        len: usize,
        prot: c_int,
        how: c_int,
        fd: c_int,
        o: c_long,
    ) -> *mut c_void;
    fn prctl(option: c_int, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> c_int;
    fn madvise(addr: *mut c_void, len: usize, advice: c_int) -> c_int;
}

/// Drops this process's pages for the whole pages inside `slice`, which is done
/// with. At exit the kernel would otherwise unmap the 58,000 pages of the input one
/// by one, on one core (about 9 ms); this does the same work in each parsing thread,
/// in parallel. Reading the input again is still correct: the page faults back in
/// from the page cache. Anonymous memory (the records) is freed and would read as
/// zeros, but it is never read again.
pub fn release<T>(slice: &[T]) {
    const PAGE: usize = 4096;
    let start = (slice.as_ptr() as usize).next_multiple_of(PAGE);
    let end = (slice.as_ptr() as usize + size_of_val(slice)) / PAGE * PAGE;
    if end > start {
        // SAFETY: whole pages of a private mapping whose contents are no longer
        // needed (see above); no reference observes a change. 4 is MADV_DONTNEED.
        unsafe { madvise(start as *mut c_void, end - start, 4) };
    }
}

/// Turns off transparent huge pages for this process. With THP set to "always",
/// touching one byte of a sparse, lazily zeroed table can fault in a whole 2 MB
/// page, so the memory peak would depend on how fragmented the host is that run.
pub fn no_huge_pages() {
    // SAFETY: a plain syscall wrapper; failure only means THP stays on. 41 is
    // PR_SET_THP_DISABLE.
    unsafe { prctl(41, 1, 0, 0, 0) };
}

const READ: c_int = 1;
const WRITE: c_int = 2;
const SHARED: c_int = 1;
const PRIVATE: c_int = 2;
const FIXED: c_int = 0x10;
const ANONYMOUS: c_int = 0x20;

/// Zero bytes mapped after the end of the file, so that fixed-size SIMD loads at
/// any line of it stay inside the mapping.
pub const PAD: usize = 256;

/// mmap at file offset 0 with protection, flags and file `[prot, how, fd]`.
fn map_at(at: *mut u8, len: usize, [prot, how, fd]: [c_int; 3]) -> *mut u8 {
    // SAFETY: each caller maps fresh memory or replaces its own reservation.
    let ptr = unsafe { mmap(at.cast(), len, prot, how, fd, 0) };
    assert_ne!(ptr as isize, -1, "mmap: {}", Error::last_os_error());
    ptr.cast()
}

/// Maps the whole file read-only, followed by PAD zero bytes: the whole range is
/// reserved as zero pages, then the file is mapped over its start (the rest of the
/// file's last page reads as zero). The dataset is mounted read-only, so the bytes
/// cannot change under us.
pub fn map(file: &File) -> &'static [u8] {
    let len = file.metadata().expect("stat the log").len() as usize;
    let base = map_at(null_mut(), len + PAD, [READ, PRIVATE | ANONYMOUS, -1]);
    if len > 0 {
        map_at(base, len, [READ, PRIVATE | FIXED, file.as_raw_fd()]);
    }
    // SAFETY: `len + PAD` bytes are mapped and stay mapped until exit.
    unsafe { std::slice::from_raw_parts(base, len + PAD) }
}

/// Maps the first `len` (> 0) bytes of a file opened for reading and writing,
/// shared: writing the memory writes the file.
pub fn map_mut(file: &File, len: usize) -> *mut u8 {
    map_at(null_mut(), len, [READ | WRITE, SHARED, file.as_raw_fd()])
}
