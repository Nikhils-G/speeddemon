//! Read-only mmap through libc, declared by hand so the crate has no dependencies.

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::raw::{c_int, c_long, c_void};

unsafe extern "C" {
    fn mmap(addr: *mut c_void, len: usize, prot: c_int, flags: c_int, fd: c_int, offset: c_long) -> *mut c_void;
    fn prctl(option: c_int, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> c_int;
}

const PR_SET_THP_DISABLE: c_int = 41;
const PROT_READ: c_int = 1;
const MAP_PRIVATE: c_int = 2;
const MAP_FIXED: c_int = 0x10;
const MAP_ANONYMOUS: c_int = 0x20;

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

/// Maps the whole file read-only, followed by PAD zero bytes, for the rest of the
/// process. It is never unmapped: the process exits right after using it.
pub fn map(file: &File) -> io::Result<&'static [u8]> {
    let len = file.metadata()?.len() as usize;
    // Reserve the whole range as zero pages first, then map the file over its start.
    // SAFETY: a fresh anonymous read-only mapping; nothing else refers to it.
    let base = unsafe { mmap(std::ptr::null_mut(), len + PAD, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) };
    if base as isize == -1 {
        return Err(io::Error::last_os_error());
    }
    if len > 0 {
        // SAFETY: MAP_FIXED only replaces pages of the reservation made above. The
        // dataset is mounted read-only, so the bytes cannot change under us.
        let ptr = unsafe { mmap(base, len, PROT_READ, MAP_PRIVATE | MAP_FIXED, file.as_raw_fd(), 0) };
        if ptr as isize == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    // SAFETY: `len + PAD` bytes are mapped (the file, then zeros: the tail of the
    // file's last page reads as zero) and stay mapped until exit.
    Ok(unsafe { std::slice::from_raw_parts(base as *const u8, len + PAD) })
}
