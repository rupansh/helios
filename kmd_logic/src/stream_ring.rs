//! Region bookkeeping for a per-context host-visible command-stream ring.
//!
//! The render-server proxy carries one `SUBMIT_3D` inline payload per
//! SOCK_SEQPACKET datagram, capped at the socket's default SO_SNDBUF
//! (212992 B measured 2026-09-03); a larger inline stream kills the host
//! context. Streams above the inline cap are copied into a HOST3D shmem the
//! context has attached and submitted as `vkExecuteCommandStreamsMESA`, the
//! way stock venus goes indirect. This module owns only the offsets: a FIFO
//! ring where regions are reserved at submit and retired in submission order
//! when the wire fence completes. It never waits and never allocates.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub offset: u64,
    pub len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReserveRefusal {
    /// Zero bytes, or more than the whole ring can ever hold.
    Unrepresentable,
    /// Live regions leave no contiguous run of that size; retire first.
    Full,
}

/// Alignment of every region start; the host reads the stream with plain
/// loads, so this is only about keeping sub-ring copies cache-line clean.
pub const REGION_ALIGN: u64 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamRing {
    size: u64,
    /// Next byte to hand out.
    head: u64,
    /// Oldest live byte; equals `head` when nothing is live.
    tail: u64,
    /// Bytes reserved but not retired. Distinguishes empty from full when
    /// `head == tail`, and bounds `retire` against a stale caller.
    live: u64,
}

impl StreamRing {
    /// `size` must be a multiple of [`REGION_ALIGN`]; a zero or misaligned
    /// size makes a ring that refuses every reservation.
    pub const fn new(size: u64) -> Self {
        let size = if size % REGION_ALIGN == 0 { size } else { 0 };
        Self {
            size,
            head: 0,
            tail: 0,
            live: 0,
        }
    }

    pub const fn size(&self) -> u64 {
        self.size
    }

    pub const fn live_bytes(&self) -> u64 {
        self.live
    }

    /// Reserve `len` contiguous bytes. A region never wraps: when the run to
    /// the end of the ring is too short the remainder is skipped (counted as
    /// live so `retire` walks past it in order) and the region starts at 0.
    pub fn reserve(&mut self, len: u64) -> Result<Region, ReserveRefusal> {
        if len == 0 || len > self.size {
            return Err(ReserveRefusal::Unrepresentable);
        }
        let padded = len.div_ceil(REGION_ALIGN) * REGION_ALIGN;
        if padded > self.size {
            return Err(ReserveRefusal::Unrepresentable);
        }
        let free = self.size - self.live;
        let to_end = self.size - self.head;
        if padded <= to_end {
            if padded > free {
                return Err(ReserveRefusal::Full);
            }
            let offset = self.head;
            self.head = (self.head + padded) % self.size;
            self.live += padded;
            return Ok(Region { offset, len });
        }
        // Wrap: the tail run [head, size) is dead space this reservation owns.
        if to_end + padded > free {
            return Err(ReserveRefusal::Full);
        }
        self.live += to_end + padded;
        self.head = padded % self.size;
        Ok(Region { offset: 0, len })
    }

    /// Retire the OLDEST live region. `region` must be the value `reserve`
    /// returned for it, in reservation order; anything else is refused and
    /// leaves the ring untouched, so a caller bug cannot free live bytes.
    pub fn retire(&mut self, region: Region) -> bool {
        if region.len == 0 || self.live == 0 {
            return false;
        }
        let padded = region.len.div_ceil(REGION_ALIGN) * REGION_ALIGN;
        let mut consumed = 0u64;
        if region.offset == 0 && self.tail != 0 {
            // The reservation wrapped: it owns the dead run [tail, size).
            consumed = self.size - self.tail;
        } else if region.offset != self.tail {
            return false;
        }
        consumed += padded;
        if consumed > self.live {
            return false;
        }
        self.live -= consumed;
        self.tail = (region.offset + padded) % self.size;
        if self.live == 0 {
            // Empty: restart from 0 so the next region need not wrap.
            self.head = 0;
            self.tail = 0;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_come_back_in_order_and_free_in_order() {
        let mut ring = StreamRing::new(1024);
        let a = ring.reserve(100).unwrap();
        let b = ring.reserve(200).unwrap();
        assert_eq!(a, Region { offset: 0, len: 100 });
        assert_eq!(b, Region { offset: 128, len: 200 });
        assert_eq!(ring.live_bytes(), 128 + 256);
        assert!(!ring.retire(b), "out of order retire must be refused");
        assert!(ring.retire(a));
        assert!(ring.retire(b));
        assert_eq!(ring.live_bytes(), 0);
    }

    #[test]
    fn a_region_never_wraps_and_the_skipped_tail_is_reclaimed() {
        let mut ring = StreamRing::new(512);
        let a = ring.reserve(300).unwrap(); // [0, 320)
        let b = ring.reserve(100).unwrap(); // [320, 448)
        assert_eq!(b.offset, 320);
        assert!(ring.retire(a));
        // 200 needs 256 > the 64 left at the end: wraps to 0, owning [448,512).
        let c = ring.reserve(200).unwrap();
        assert_eq!(c.offset, 0);
        assert_eq!(ring.live_bytes(), 128 + 64 + 256);
        assert!(ring.retire(b));
        assert!(ring.retire(c));
        assert_eq!(ring.live_bytes(), 0);
    }

    #[test]
    fn full_is_a_refusal_not_a_corruption() {
        let mut ring = StreamRing::new(256);
        let a = ring.reserve(256).unwrap();
        assert_eq!(ring.reserve(1), Err(ReserveRefusal::Full));
        assert_eq!(ring.reserve(257), Err(ReserveRefusal::Unrepresentable));
        assert_eq!(ring.reserve(0), Err(ReserveRefusal::Unrepresentable));
        assert!(ring.retire(a));
        assert!(ring.reserve(256).is_ok());
    }

    #[test]
    fn a_wrapped_reservation_cannot_overrun_the_live_tail() {
        let mut ring = StreamRing::new(512);
        let a = ring.reserve(64).unwrap(); // [0,64)
        let _b = ring.reserve(384).unwrap(); // [64,448)
        assert!(ring.retire(a));
        // 128 does not fit in [448,512); wrapping needs 64 + 128 = 192 free,
        // and only 64 + 64 = 128 are free.
        assert_eq!(ring.reserve(128), Err(ReserveRefusal::Full));
        assert_eq!(ring.reserve(64).unwrap().offset, 448);
    }

    #[test]
    fn a_misaligned_or_zero_size_ring_refuses_everything() {
        assert_eq!(StreamRing::new(100).reserve(1), Err(ReserveRefusal::Unrepresentable));
        assert_eq!(StreamRing::new(0).reserve(1), Err(ReserveRefusal::Unrepresentable));
    }
}
