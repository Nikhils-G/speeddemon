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
const MAP_PRIVATE: c_int = 2;

/// Maps the whole file read-only for the rest of the process. It is never
/// unmapped: the process exits right after using it, and exit unmaps everything.
pub fn map(file: &File) -> io::Result<&'static [u8]> {
    let len = file.metadata()?.len() as usize;
    if len == 0 {
        return Ok(&[]);
    }
    // SAFETY: a fresh read-only private mapping of `len` bytes; the dataset is
    // mounted read-only, so the bytes cannot change under us.
    let ptr = unsafe {
        mmap(
            std::ptr::null_mut(),
            len,
            PROT_READ,
            MAP_PRIVATE,
            file.as_raw_fd(),
            0,
        )
    };
    if ptr as isize == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the mapping is valid for `len` bytes and lives until exit.
    Ok(unsafe { std::slice::from_raw_parts(ptr as *const u8, len) })
}
