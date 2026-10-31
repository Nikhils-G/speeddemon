//! Read-only mmap through libc, declared by hand so the crate has no dependencies.

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::raw::{c_int, c_long, c_void};

unsafe extern "C" {
    fn mmap(
        addr: *mut c_void,
        len: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        offset: c_long,
    ) -> *mut c_void;
    fn prctl(option: c_int, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> c_int;
    fn madvise(addr: *mut c_void, len: usize, advice: c_int) -> c_int;
}

/// Drops this process's page-table entries for the whole pages inside `slice`, a
/// part of a read-only file mapping that is done with. At exit the kernel would
/// otherwise unmap the 58,000 pages of the input one by one, on one core (about
/// 9 ms); this does the same work in each parsing thread, in parallel. Reading the
/// bytes again is still correct: the page faults back in from the page cache.
pub fn release(slice: &[u8]) {
    const PAGE: usize = 4096;
    const MADV_DONTNEED: c_int = 4;
    let start = (slice.as_ptr() as usize).next_multiple_of(PAGE);
    let end = (slice.as_ptr() as usize + slice.len()) / PAGE * PAGE;
    if end > start {
        // SAFETY: whole pages inside a private, read-only file mapping that is
        // never written; dropping them loses nothing.
        unsafe { madvise(start as *mut c_void, end - start, MADV_DONTNEED) };
    }
}

const PR_SET_THP_DISABLE: c_int = 41;

/// Turns off transparent huge pages for this process. With THP set to "always",
/// touching one byte of a sparse, lazily zeroed table can fault in a whole 2 MB
/// page, so the memory peak would depend on how fragmented the host is that run.
pub fn no_huge_pages() {
    // SAFETY: a plain syscall wrapper; failure only means THP stays on.
    unsafe { prctl(PR_SET_THP_DISABLE, 1, 0, 0, 0) };
}

const PROT_READ: c_int = 1;
const PROT_WRITE: c_int = 2;
const MAP_SHARED: c_int = 1;
const MAP_PRIVATE: c_int = 2;
const MAP_FIXED: c_int = 0x10;
const MAP_ANONYMOUS: c_int = 0x20;

/// Zero bytes mapped after the end of the file, so that fixed-size SIMD loads at
/// any line of it stay inside the mapping.
pub const PAD: usize = 256;

/// Maps the whole file read-only, followed by PAD zero bytes, for the rest of the
/// process. It is never unmapped: the process exits right after using it.
pub fn map(file: &File) -> io::Result<&'static [u8]> {
    let len = file.metadata()?.len() as usize;
    // Reserve the whole range as zero pages, then map the file over its start.
    // SAFETY: a fresh anonymous read-only mapping; nothing else refers to it.
    let base = unsafe {
        mmap(
            std::ptr::null_mut(),
            len + PAD,
            PROT_READ,
            MAP_PRIVATE | MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if base as isize == -1 {
        return Err(io::Error::last_os_error());
    }
    if len > 0 {
        // SAFETY: MAP_FIXED only replaces pages of the reservation made above; the
        // dataset is mounted read-only, so the bytes cannot change under us.
        let ptr = unsafe {
            mmap(
                base,
                len,
                PROT_READ,
                MAP_PRIVATE | MAP_FIXED,
                file.as_raw_fd(),
                0,
            )
        };
        if ptr as isize == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    // SAFETY: `len + PAD` bytes are mapped (the file, then zeros: the rest of the
    // file's last page reads as zero) and stay mapped until exit.
    Ok(unsafe { std::slice::from_raw_parts(base as *const u8, len + PAD) })
}

/// Maps the first `len` bytes of a file opened for reading and writing, shared:
/// writing the slice writes the file. Never unmapped, like `map`.
pub fn map_mut(file: &File, len: usize) -> io::Result<&'static mut [u8]> {
    if len == 0 {
        return Ok(&mut []);
    }
    // SAFETY: a fresh shared mapping of `len` bytes of a file this process just
    // created and sized; nothing else maps or writes it.
    let ptr = unsafe {
        mmap(
            std::ptr::null_mut(),
            len,
            PROT_READ | PROT_WRITE,
            MAP_SHARED,
            file.as_raw_fd(),
            0,
        )
    };
    if ptr as isize == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the mapping is valid for `len` bytes, lives until exit, and this is
    // the only reference to it.
    Ok(unsafe { std::slice::from_raw_parts_mut(ptr as *mut u8, len) })
}
