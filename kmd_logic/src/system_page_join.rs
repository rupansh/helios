//! System-page join for a present destination: VidMm homes the compositor's
//! staging surface in system memory (allocation-list SegmentId 0) and maps
//! those pages to the GPU through page-table updates; dwm's CPU reads them
//! while the present copy wrote the host blob — two buffers. The join imports
//! the mapped pages as a guest blob and points the copy at them. Measured
//! 2026-09-03: every windowed BLT swapchain froze on its first frame; the host
//! blob sampled fresh every copy while the window never changed.

use helios_protocol::{VirtioGpuMemEntry, HELIOS_CPU_BACKING_HOST_GRANULARITY};

pub const PAGE: u64 = 4096;
/// `HELIOS_HWA2_KIND_STANDARD_STAGING` — the one kind the compositor CPU-reads.
pub const STANDARD_STAGING_KIND: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum JoinRefusal {
    NotStaging = 1,
    NoHostBlob = 2,
    AlreadyJoined = 3,
    Offset = 4,
    Short = 5,
    Runs = 6,
    Pfn = 7,
    NoMdl = 8,
    Empty = 9,
}

#[derive(Debug, Clone, Copy)]
pub struct JoinRequest {
    pub allocation_kind: u32,
    pub host_blob: bool,
    pub joined: bool,
    pub byte_size: u64,
    pub offset_in_pages: u64,
    pub number_of_pages: u64,
    pub pages_present: bool,
}

/// The guest-blob size (host granularity multiple) the mapped pages can back,
/// or why they cannot. The range must start at the allocation and cover the
/// whole surface: the copy alias spans all of it.
pub fn admit(req: &JoinRequest) -> Result<u64, JoinRefusal> {
    if req.allocation_kind != STANDARD_STAGING_KIND {
        return Err(JoinRefusal::NotStaging);
    }
    if !req.host_blob {
        return Err(JoinRefusal::NoHostBlob);
    }
    if req.joined {
        return Err(JoinRefusal::AlreadyJoined);
    }
    if !req.pages_present {
        return Err(JoinRefusal::NoMdl);
    }
    if req.byte_size == 0 {
        return Err(JoinRefusal::Empty);
    }
    if req.offset_in_pages != 0 {
        return Err(JoinRefusal::Offset);
    }
    let size = req
        .byte_size
        .checked_add(HELIOS_CPU_BACKING_HOST_GRANULARITY - 1)
        .ok_or(JoinRefusal::Short)?
        & !(HELIOS_CPU_BACKING_HOST_GRANULARITY - 1);
    if req.number_of_pages.saturating_mul(PAGE) < size {
        return Err(JoinRefusal::Short);
    }
    Ok(size)
}

/// Coalesce page frame numbers into contiguous guest-physical runs. Returns the
/// run count; refuses more runs than `out` holds (the host's udmabuf list
/// limit) or a PFN that does not fit a 64-bit byte address.
pub fn coalesce(pfns: &[u64], out: &mut [VirtioGpuMemEntry]) -> Result<usize, JoinRefusal> {
    let mut runs = 0usize;
    for &pfn in pfns {
        if pfn > (u64::MAX >> 12) {
            return Err(JoinRefusal::Pfn);
        }
        let address = pfn << 12;
        if runs != 0 {
            let last = &mut out[runs - 1];
            if last.addr + last.length as u64 == address && last.length <= u32::MAX - PAGE as u32 {
                last.length += PAGE as u32;
                continue;
            }
        }
        if runs == out.len() {
            return Err(JoinRefusal::Runs);
        }
        out[runs] = VirtioGpuMemEntry {
            addr: address,
            length: PAGE as u32,
            padding: 0,
        };
        runs += 1;
    }
    Ok(runs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn staging(byte_size: u64, pages: u64) -> JoinRequest {
        JoinRequest {
            allocation_kind: STANDARD_STAGING_KIND,
            host_blob: true,
            joined: false,
            byte_size,
            offset_in_pages: 0,
            number_of_pages: pages,
            pages_present: true,
        }
    }

    #[test]
    fn whole_map_rounds_to_host_granularity() {
        // A dwm redirection surface measured 2026-09-02: 761 pages charged 768.
        assert_eq!(admit(&staging(3_117_056, 768)), Ok(3_145_728));
        assert_eq!(admit(&staging(65_536, 16)), Ok(65_536));
    }

    #[test]
    fn short_partial_or_foreign_maps_are_refused() {
        assert_eq!(admit(&staging(3_117_056, 761)), Err(JoinRefusal::Short));
        let mut r = staging(65_536, 16);
        r.offset_in_pages = 1;
        assert_eq!(admit(&r), Err(JoinRefusal::Offset));
        let mut r = staging(65_536, 16);
        r.allocation_kind = 1;
        assert_eq!(admit(&r), Err(JoinRefusal::NotStaging));
        let mut r = staging(65_536, 16);
        r.host_blob = false;
        assert_eq!(admit(&r), Err(JoinRefusal::NoHostBlob));
        let mut r = staging(65_536, 16);
        r.joined = true;
        assert_eq!(admit(&r), Err(JoinRefusal::AlreadyJoined));
        let mut r = staging(65_536, 16);
        r.pages_present = false;
        assert_eq!(admit(&r), Err(JoinRefusal::NoMdl));
        assert_eq!(admit(&staging(0, 16)), Err(JoinRefusal::Empty));
    }

    #[test]
    fn coalesce_merges_contiguous_pages_only() {
        let zero = VirtioGpuMemEntry { addr: 0, length: 0, padding: 0 };
        let mut out = [zero; 4];
        let runs = coalesce(&[0x100, 0x101, 0x102, 0x200, 0x300, 0x301], &mut out).unwrap();
        assert_eq!(runs, 3);
        assert_eq!((out[0].addr, out[0].length), (0x100_000, 3 * 4096));
        assert_eq!((out[1].addr, out[1].length), (0x200_000, 4096));
        assert_eq!((out[2].addr, out[2].length), (0x300_000, 2 * 4096));
    }

    #[test]
    fn coalesce_refuses_too_many_runs_and_bad_pfns() {
        let zero = VirtioGpuMemEntry { addr: 0, length: 0, padding: 0 };
        let mut out = [zero; 2];
        assert_eq!(coalesce(&[1, 3, 5], &mut out), Err(JoinRefusal::Runs));
        assert_eq!(coalesce(&[u64::MAX], &mut out), Err(JoinRefusal::Pfn));
        assert_eq!(coalesce(&[], &mut out), Ok(0));
    }
}
