//! mmap, madvise and prctl through libc, declared by hand so the crate has no
//! dependencies.

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

/// Gives back the whole pages inside `slice`, which is done with: anonymous memory
/// (the records) is freed and would read as zeros, but it is never read again.
pub fn release(slice: &[u8]) {
    const PAGE: usize = 4096;
    const MADV_DONTNEED: c_int = 4;
    let start = (slice.as_ptr() as usize).next_multiple_of(PAGE);
    let end = (slice.as_ptr() as usize + slice.len()) / PAGE * PAGE;
    if end > start {
        // SAFETY: whole pages of a private mapping whose contents are no longer
        // needed (see above); no reference observes a change.
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
