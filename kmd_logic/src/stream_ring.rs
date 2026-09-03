//! Region bookkeeping for a per-context host-visible command-stream ring.
//!
//! The render-server proxy carries one `SUBMIT_3D` inline payload per
//! SOCK_SEQPACKET datagram, capped at the socket's default SO_SNDBUF
//! (212992 B measured 2026-09-03); a larger inline stream kills the host
//! context. Streams above the inline cap are copied into a HOST3D shmem the
//! context has attached and submitted as `vkExecuteCommandStreamsMESA`, the
//! way stock venus goes indirect. This module owns only the offsets: regions
//! are reserved at submit and released when their submission terminates.
//! Terminations may arrive out of order (a failed enqueue retires the newest
//! region while older ones are live), so a region is *marked* done and the
//! tail advances over the leading done regions. Never waits, never allocates.

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
    /// As many regions live as the queue can track.
    QueueFull,
}

/// Alignment of every region start; the host reads the stream with plain
/// loads, so this is only about keeping sub-ring copies cache-line clean.
pub const REGION_ALIGN: u64 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    region: Region,
    /// Bytes this reservation owns: the padded region plus any skipped tail
    /// run it wrapped past.
    owned: u64,
    done: bool,
}

const EMPTY: Entry = Entry {
    region: Region { offset: 0, len: 0 },
    owned: 0,
    done: false,
};

/// `CAP` bounds the live regions (the transport's in-flight cap suffices).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamRing<const CAP: usize> {
    size: u64,
    /// Next byte to hand out.
    head: u64,
    /// Oldest live byte; equals `head` when nothing is live.
    tail: u64,
    /// Bytes reserved but not released.
    live: u64,
    queue: [Entry; CAP],
    first: usize,
    count: usize,
}

impl<const CAP: usize> StreamRing<CAP> {
    /// `size` must be a multiple of [`REGION_ALIGN`]; a zero or misaligned
    /// size makes a ring that refuses every reservation.
    pub const fn new(size: u64) -> Self {
        let size = if size % REGION_ALIGN == 0 { size } else { 0 };
        Self {
            size,
            head: 0,
            tail: 0,
            live: 0,
            queue: [EMPTY; CAP],
            first: 0,
            count: 0,
        }
    }

    pub const fn size(&self) -> u64 {
        self.size
    }

    pub const fn live_bytes(&self) -> u64 {
        self.live
    }

    pub const fn live_regions(&self) -> usize {
        self.count
    }

    /// Reserve `len` contiguous bytes. A region never wraps: when the run to
    /// the end of the ring is too short the remainder is skipped (owned by
    /// this reservation so the tail walks past it) and the region starts at 0.
    pub fn reserve(&mut self, len: u64) -> Result<Region, ReserveRefusal> {
        if len == 0 || len > self.size {
            return Err(ReserveRefusal::Unrepresentable);
        }
        let padded = len.div_ceil(REGION_ALIGN) * REGION_ALIGN;
        if padded > self.size {
            return Err(ReserveRefusal::Unrepresentable);
        }
        if CAP == 0 || self.count >= CAP {
            return Err(ReserveRefusal::QueueFull);
        }
        let free = self.size - self.live;
        let to_end = self.size - self.head;
        let (offset, owned) = if padded <= to_end {
            if padded > free {
                return Err(ReserveRefusal::Full);
            }
            (self.head, padded)
        } else {
            if to_end + padded > free {
                return Err(ReserveRefusal::Full);
            }
            (0, to_end + padded)
        };
        self.head = (offset + padded) % self.size;
        self.live += owned;
        let slot = (self.first + self.count) % CAP;
        self.queue[slot] = Entry {
            region: Region { offset, len },
            owned,
            done: false,
        };
        self.count += 1;
        Ok(Region { offset, len })
    }

    /// Release a live region. It must be a value `reserve` returned and not
    /// yet retired; anything else is refused and leaves the ring untouched,
    /// so a caller bug cannot free live bytes. The tail advances over every
    /// leading done region, so out-of-order releases only defer space.
    pub fn retire(&mut self, region: Region) -> bool {
        if region.len == 0 {
            return false;
        }
        let mut found = false;
        for i in 0..self.count {
            let slot = (self.first + i) % CAP;
            let entry = &mut self.queue[slot];
            if !entry.done && entry.region == region {
                entry.done = true;
                found = true;
                break;
            }
        }
        if !found {
            return false;
        }
        while self.count > 0 && self.queue[self.first].done {
            let entry = self.queue[self.first];
            self.queue[self.first] = EMPTY;
            self.first = (self.first + 1) % CAP;
            self.count -= 1;
            self.live -= entry.owned;
            let padded = entry.region.len.div_ceil(REGION_ALIGN) * REGION_ALIGN;
            self.tail = (entry.region.offset + padded) % self.size;
        }
        if self.count == 0 {
            // Empty: restart from 0 so the next region need not wrap.
            self.head = 0;
            self.tail = 0;
            self.live = 0;
        }
        true
    }
}

/// `VK_COMMAND_TYPE_vkExecuteCommandStreamsMESA_EXT`.
pub const CMD_EXECUTE_COMMAND_STREAMS_MESA: u32 = 180;

/// Bytes of one `vkExecuteCommandStreamsMESA` naming a single stream and no
/// replies or dependencies: type, flags, streamCount, array_size(1),
/// {resourceId, offset, size}, array_size(0), dependencyCount, array_size(0),
/// flags. Every array size and size_t is a u64 on the wire.
pub const EXECUTE_STUB_BYTES: usize = 64;

/// The inline SUBMIT_3D that executes `size` bytes at `offset` of the ring
/// resource `resource_id` (venus `vn_encode_vkExecuteCommandStreamsMESA`).
pub fn execute_stub(resource_id: u32, offset: u64, size: u64) -> [u8; EXECUTE_STUB_BYTES] {
    let mut out = [0u8; EXECUTE_STUB_BYTES];
    let mut at = 0usize;
    let mut put = |bytes: &[u8]| {
        out[at..at + bytes.len()].copy_from_slice(bytes);
        at += bytes.len();
    };
    put(&CMD_EXECUTE_COMMAND_STREAMS_MESA.to_le_bytes());
    put(&0u32.to_le_bytes()); // VkCommandFlagsEXT
    put(&1u32.to_le_bytes()); // streamCount
    put(&1u64.to_le_bytes()); // array_size(pStreams)
    put(&resource_id.to_le_bytes());
    put(&offset.to_le_bytes());
    put(&size.to_le_bytes());
    put(&0u64.to_le_bytes()); // array_size(pReplyPositions) = NULL
    put(&0u32.to_le_bytes()); // dependencyCount
    put(&0u64.to_le_bytes()); // array_size(pDependencies) = NULL
    put(&0u32.to_le_bytes()); // VkCommandStreamExecutionFlagsMESA
    debug_assert_eq!(at, EXECUTE_STUB_BYTES);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    type Ring = StreamRing<8>;

    #[test]
    fn the_execute_stub_is_the_documented_64_wire_bytes() {
        let stub = execute_stub(0x1234_5678, 0x0000_0001_0000_0040, 418_652);
        let mut at = 0usize;
        let mut expect = |bytes: &[u8]| {
            assert_eq!(&stub[at..at + bytes.len()], bytes, "at byte {at}");
            at += bytes.len();
        };
        expect(&180u32.to_le_bytes()); // vkExecuteCommandStreamsMESA
        expect(&0u32.to_le_bytes()); // cmd flags
        expect(&1u32.to_le_bytes()); // streamCount
        expect(&1u64.to_le_bytes()); // array_size(pStreams)
        expect(&0x1234_5678u32.to_le_bytes()); // resourceId
        expect(&0x0000_0001_0000_0040u64.to_le_bytes()); // offset
        expect(&418_652u64.to_le_bytes()); // size
        expect(&0u64.to_le_bytes()); // pReplyPositions = NULL
        expect(&0u32.to_le_bytes()); // dependencyCount
        expect(&0u64.to_le_bytes()); // pDependencies = NULL
        expect(&0u32.to_le_bytes()); // execution flags
        assert_eq!(at, EXECUTE_STUB_BYTES);
    }

    #[test]
    fn regions_come_back_in_order_and_free_in_order() {
        let mut ring = Ring::new(1024);
        let a = ring.reserve(100).unwrap();
        let b = ring.reserve(200).unwrap();
        assert_eq!(a, Region { offset: 0, len: 100 });
        assert_eq!(b, Region { offset: 128, len: 200 });
        assert_eq!(ring.live_bytes(), 128 + 256);
        assert!(ring.retire(a));
        assert_eq!(ring.live_bytes(), 256);
        assert!(ring.retire(b));
        assert_eq!(ring.live_bytes(), 0);
        assert_eq!(ring.live_regions(), 0);
    }

    #[test]
    fn an_out_of_order_release_defers_space_until_the_older_region_goes() {
        let mut ring = Ring::new(1024);
        let a = ring.reserve(100).unwrap();
        let b = ring.reserve(200).unwrap();
        // The newest fails at enqueue and is released first.
        assert!(ring.retire(b));
        assert_eq!(ring.live_bytes(), 128 + 256, "space stays owned behind a");
        assert!(!ring.retire(b), "a second release of the same region is refused");
        assert!(ring.retire(a));
        assert_eq!(ring.live_bytes(), 0);
        assert_eq!(ring.live_regions(), 0);
    }

    #[test]
    fn a_region_never_wraps_and_the_skipped_tail_is_reclaimed() {
        let mut ring = Ring::new(512);
        let a = ring.reserve(300).unwrap(); // [0, 320)
        let b = ring.reserve(100).unwrap(); // [320, 448)
        assert_eq!(b.offset, 320);
        assert!(ring.retire(a));
        // 200 needs 256 > the 64 left at the end: wraps to 0, owning [448,512).
        let c = ring.reserve(200).unwrap();
        assert_eq!(c.offset, 0);
        assert_eq!(ring.live_bytes(), 128 + 64 + 256);
        assert!(ring.retire(b));
        // c still owns the 64-byte tail run it skipped plus its 256.
        assert_eq!(ring.live_bytes(), 64 + 256);
        assert!(ring.retire(c));
        assert_eq!(ring.live_bytes(), 0);
    }

    #[test]
    fn full_is_a_refusal_not_a_corruption() {
        let mut ring = Ring::new(256);
        let a = ring.reserve(256).unwrap();
        assert_eq!(ring.reserve(1), Err(ReserveRefusal::Full));
        assert_eq!(ring.reserve(257), Err(ReserveRefusal::Unrepresentable));
        assert_eq!(ring.reserve(0), Err(ReserveRefusal::Unrepresentable));
        assert!(ring.retire(a));
        assert!(ring.reserve(256).is_ok());
    }

    #[test]
    fn a_wrapped_reservation_cannot_overrun_the_live_tail() {
        let mut ring = Ring::new(512);
        let a = ring.reserve(64).unwrap(); // [0,64)
        let _b = ring.reserve(384).unwrap(); // [64,448)
        assert!(ring.retire(a));
        // 128 does not fit in [448,512); wrapping needs 64 + 128 = 192 free,
        // and only 64 + 64 = 128 are free.
        assert_eq!(ring.reserve(128), Err(ReserveRefusal::Full));
        assert_eq!(ring.reserve(64).unwrap().offset, 448);
    }

    #[test]
    fn the_queue_bounds_live_regions_and_a_stale_release_is_refused() {
        let mut ring = StreamRing::<2>::new(1024);
        let a = ring.reserve(64).unwrap();
        let _b = ring.reserve(64).unwrap();
        assert_eq!(ring.reserve(64), Err(ReserveRefusal::QueueFull));
        assert!(!ring.retire(Region { offset: 999, len: 1 }));
        assert!(ring.retire(a));
        assert!(ring.reserve(64).is_ok());
    }

    #[test]
    fn a_misaligned_or_zero_size_ring_refuses_everything() {
        assert_eq!(Ring::new(100).reserve(1), Err(ReserveRefusal::Unrepresentable));
        assert_eq!(Ring::new(0).reserve(1), Err(ReserveRefusal::Unrepresentable));
    }
}
