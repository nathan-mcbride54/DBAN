//! Page-aligned I/O buffers.
//!
//! `O_DIRECT` block-device I/O on Linux requires the user buffer to be aligned
//! to the logical sector size (and in practice to the page size). A plain
//! `Vec<u8>` gives no alignment guarantee, so we allocate explicitly.

use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

/// Alignment used for all engine buffers. 4096 satisfies every logical sector
/// size in the wild (512, 520, 4096) as well as the page-alignment O_DIRECT
/// historically wanted.
pub const ALIGNMENT: usize = 4096;

pub struct AlignedBuf {
    ptr: NonNull<u8>,
    len: usize,
}

// The buffer is a plain owned allocation; moving it across threads is safe.
unsafe impl Send for AlignedBuf {}

impl AlignedBuf {
    /// Allocate a zero-filled aligned buffer. `len` must be non-zero.
    pub fn zeroed(len: usize) -> Self {
        assert!(len > 0, "AlignedBuf must have non-zero length");
        let layout = Layout::from_size_align(len, ALIGNMENT).expect("invalid buffer layout");
        // SAFETY: layout has non-zero size (asserted above).
        let raw = unsafe { alloc_zeroed(layout) };
        let ptr = NonNull::new(raw).expect("buffer allocation failed");
        AlignedBuf { ptr, len }
    }
}

impl Deref for AlignedBuf {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        // SAFETY: ptr is valid for len bytes for the life of self.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

impl DerefMut for AlignedBuf {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: ptr is valid for len bytes for the life of self; &mut self
        // guarantees exclusive access.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        let layout = Layout::from_size_align(self.len, ALIGNMENT).expect("invalid buffer layout");
        // SAFETY: allocated with the identical layout in `zeroed`.
        unsafe { dealloc(self.ptr.as_ptr(), layout) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alignment_and_zeroing() {
        let buf = AlignedBuf::zeroed(1 << 20);
        assert_eq!(buf.as_ptr() as usize % ALIGNMENT, 0);
        assert_eq!(buf.len(), 1 << 20);
        assert!(buf.iter().all(|&b| b == 0));
    }

    #[test]
    fn writable() {
        let mut buf = AlignedBuf::zeroed(4096);
        buf[0] = 0xAB;
        buf[4095] = 0xCD;
        assert_eq!(buf[0], 0xAB);
        assert_eq!(buf[4095], 0xCD);
    }
}
