//! Process-local CPU backing for an exact outer WDDM allocation.
//!
//! The pointer is a data-access view, never an allocation identity or lookup
//! key. Ownership stays on the outer UMD's device graph until the matching
//! WDDM deallocation has succeeded.

use core::ffi::c_void;
use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::ptr::NonNull;

const WDDM_PAGE_ALIGN: usize = 4096;

pub struct CpuBacking {
    ptr: NonNull<u8>,
    layout: Layout,
}

// The allocation is plain process memory. Access synchronization is owned by
// the D3D/Vulkan resource graph; this wrapper controls only its lifetime.
unsafe impl Send for CpuBacking {}
unsafe impl Sync for CpuBacking {}

impl CpuBacking {
    pub fn new(bytes: u64) -> Option<Self> {
        let bytes = usize::try_from(bytes).ok()?;
        if bytes == 0 {
            return None;
        }
        let layout = Layout::from_size_align(bytes, WDDM_PAGE_ALIGN).ok()?;
        // SAFETY: `layout` is nonzero and valid. `Drop` deallocates with the
        // identical layout after the WDDM allocation has released pSystemMem.
        let ptr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
        Some(Self { ptr, layout })
    }

    pub fn as_ptr(&self) -> *mut c_void {
        self.ptr.as_ptr().cast()
    }

    pub fn bytes(&self) -> usize {
        self.layout.size()
    }

    /// Preserve backing after a failed WDDM deallocation. Leaking is safer
    /// than allowing dxgkrnl to retain a pointer to freed process memory.
    pub fn leak(self) {
        core::mem::forget(self);
    }
}

impl Drop for CpuBacking {
    fn drop(&mut self) {
        // SAFETY: `ptr` came from `alloc_zeroed` with this exact layout and has
        // not been deallocated elsewhere.
        unsafe { dealloc(self.ptr.as_ptr(), self.layout) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backing_is_page_aligned_and_exactly_sized() {
        let backing = CpuBacking::new(8193).unwrap();
        assert_eq!(backing.as_ptr() as usize & (WDDM_PAGE_ALIGN - 1), 0);
        assert_eq!(backing.bytes(), 8193);
    }

    #[test]
    fn zero_and_unrepresentable_sizes_are_refused() {
        assert!(CpuBacking::new(0).is_none());
        if usize::BITS < 64 {
            assert!(CpuBacking::new(u64::MAX).is_none());
        }
    }
}
