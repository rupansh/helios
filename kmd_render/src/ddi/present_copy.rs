//! D5b — the KMD present copy. A legacy BLT-model windowed present hands
//! `DxgkDdiPresent` the app's back buffer AND DWM's redirection surface and
//! expects a GPU copy; the UMD never sees the destination, so without this the
//! window is black (ROADMAP D5b, 2026-09-02).
//!
//! Three phases, one slot each: `prepare` at Present (PASSIVE: alias images,
//! the Venus stream, DMA buffers), `attach` at SubmitCommand (DISPATCH: bind the
//! K9 ticket), `issue_at_head` from the K9 drain (DISPATCH: enqueue on the KMD
//! queue's GPU ring once every earlier ticket retired, then park the ticket in
//! the compatibility FIFO behind the copy's wire fence). Every refusal counts
//! and completes the present without a copy — a black frame, never a hang.

use core::sync::atomic::{AtomicU32, Ordering};

use helios_kmd_logic::present_copy::{
    self as logic, CopyGeometry, CopyIds, CopyRect, CopyRegion, CopyToBufferIds,
    GeometryRefusal, TargetClass, MAX_REGIONS, MAX_SLOTS,
};

use crate::adapter::{AdapterContext, OrderedEngineTicket, WddmNotifyGuard};
use crate::ddi::create_allocation::{PresentCopyAllocation, PRESENT_ALIAS_REFUSED};
use crate::ddi::present_packet::PresentAllocation;
use crate::dxgk::*;
use crate::irql::PassiveLevel;
use crate::sync::SpinLock;
use crate::virtio::gpu::SUBMIT_META_BYTES;
use crate::virtio::hal::DmaBuffer;

// ── counters (`Pc*`, flushed from Present at PASSIVE) ───────────────────────
static PC_PREPARED: AtomicU32 = AtomicU32::new(0);
static PC_ISSUED: AtomicU32 = AtomicU32::new(0);
/// Copies whose wire fence retired and completed their present ticket.
static PC_DONE: AtomicU32 = AtomicU32::new(0);
/// Presents completed WITHOUT a copy after a slot had been prepared.
static PC_FALLBACK: AtomicU32 = AtomicU32::new(0);
static PC_NO_ADAPTER: AtomicU32 = AtomicU32::new(0);
static PC_NO_PAIR: AtomicU32 = AtomicU32::new(0);
static PC_SRC_REFUSED: AtomicU32 = AtomicU32::new(0);
static PC_DST_REFUSED: AtomicU32 = AtomicU32::new(0);
static PC_NO_OBJECTS: AtomicU32 = AtomicU32::new(0);
static PC_ALIAS_CREATED: AtomicU32 = AtomicU32::new(0);
static PC_ALIAS_FAILED: AtomicU32 = AtomicU32::new(0);
static PC_GEOMETRY: AtomicU32 = AtomicU32::new(0);
static PC_STRETCH: AtomicU32 = AtomicU32::new(0);
static PC_RECTS_FULL: AtomicU32 = AtomicU32::new(0);
static PC_SLOT_FULL: AtomicU32 = AtomicU32::new(0);
static PC_BUFFER_FAILED: AtomicU32 = AtomicU32::new(0);
static PC_ENCODE_FAILED: AtomicU32 = AtomicU32::new(0);
static PC_ATTACH_MISS: AtomicU32 = AtomicU32::new(0);
static PC_ENQUEUE_FAILED: AtomicU32 = AtomicU32::new(0);
static PC_RESUBMITTED: AtomicU32 = AtomicU32::new(0);
static PC_RECLAIMED: AtomicU32 = AtomicU32::new(0);
static PC_PAIR_LOGGED: AtomicU32 = AtomicU32::new(0);
static PC_FLUSH_TICKS: AtomicU32 = AtomicU32::new(0);
static PC_FLUSH_FAILURES: AtomicU32 = AtomicU32::new(0);

const fn e(name: &'static [u8], value: &'static AtomicU32) -> crate::diag::CounterEntry {
    crate::diag::CounterEntry {
        name,
        value: crate::diag::CounterRef::U32(value),
        failure: false,
    }
}
const fn f(name: &'static [u8], value: &'static AtomicU32) -> crate::diag::CounterEntry {
    crate::diag::CounterEntry {
        name,
        value: crate::diag::CounterRef::U32(value),
        failure: true,
    }
}

static PC_COUNTERS: crate::diag::CounterBlock = crate::diag::CounterBlock {
    entries: &[
        e(b"PcPrep", &PC_PREPARED),
        e(b"PcIssue", &PC_ISSUED),
        e(b"PcDone", &PC_DONE),
        f(b"PcFallback", &PC_FALLBACK),
        f(b"PcNoAdapter", &PC_NO_ADAPTER),
        e(b"PcNoPair", &PC_NO_PAIR),
        f(b"PcSrcRef", &PC_SRC_REFUSED),
        f(b"PcDstRef", &PC_DST_REFUSED),
        f(b"PcNoObj", &PC_NO_OBJECTS),
        e(b"PcAliasNew", &PC_ALIAS_CREATED),
        f(b"PcAliasFail", &PC_ALIAS_FAILED),
        f(b"PcGeom", &PC_GEOMETRY),
        f(b"PcStretch", &PC_STRETCH),
        e(b"PcRects", &PC_RECTS_FULL),
        f(b"PcSlotFull", &PC_SLOT_FULL),
        f(b"PcBufFail", &PC_BUFFER_FAILED),
        f(b"PcEncFail", &PC_ENCODE_FAILED),
        f(b"PcAttMiss", &PC_ATTACH_MISS),
        f(b"PcEnqFail", &PC_ENQUEUE_FAILED),
        e(b"PcResub", &PC_RESUBMITTED),
        e(b"PcReclaim", &PC_RECLAIMED),
    ],
    ticks: &PC_FLUSH_TICKS,
    failures: &PC_FLUSH_FAILURES,
    policy: crate::diag::FlushPolicy::EveryNth(64),
};

fn bump(counter: &AtomicU32) {
    counter.fetch_add(1, Ordering::Relaxed);
}

// ── slots ───────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotState {
    Free,
    /// Present encoded the stream; SubmitCommand has not named a ticket yet.
    Prepared { serial: u32 },
    /// Waiting for `ticket` to reach the K9 head.
    Submitted {
        serial: u32,
        ticket: OrderedEngineTicket,
    },
    /// Enqueued to the host; `ticket` completes when `fence_id` retires (a
    /// resubmission replaces the ticket). Reusable once the fence is gone.
    Issued {
        serial: u32,
        fence_id: u64,
        ticket: OrderedEngineTicket,
    },
}

struct Slot {
    state: SlotState,
    /// `(meta, venus)` between prepare and issue, or parked here after a failed
    /// enqueue so nothing is ever dropped above PASSIVE.
    buffers: Option<(DmaBuffer, DmaBuffer)>,
    stream_len: usize,
    ring_idx: u32,
}

impl Slot {
    const EMPTY: Self = Self {
        state: SlotState::Free,
        buffers: None,
        stream_len: 0,
        ring_idx: 0,
    };
}

struct Slots {
    slots: [Slot; MAX_SLOTS],
    /// Next prepare serial; never 0 (0 in the packet means "no copy").
    next_serial: u32,
}

// SAFETY: a `DmaBuffer` is an exclusively owned contiguous kernel allocation
// with no thread affinity; the lock is the only path to these slots.
unsafe impl Send for Slots {}

pub(crate) struct PresentCopyRuntime {
    slots: SpinLock<Slots>,
}

impl PresentCopyRuntime {
    pub(crate) const fn new() -> Self {
        Self {
            slots: SpinLock::new(Slots {
                slots: [Slot::EMPTY; MAX_SLOTS],
                next_serial: 1,
            }),
        }
    }
}

// ── phase 1: prepare (PASSIVE, DxgkDdiPresent) ──────────────────────────────

fn rect(r: &RECT) -> CopyRect {
    CopyRect::new(r.left, r.top, r.right, r.bottom)
}

pub(crate) fn note_no_adapter() {
    bump(&PC_NO_ADAPTER);
}

pub(crate) fn note_no_pair() {
    bump(&PC_NO_PAIR);
}

/// Prepare the copy for one BLT present. Returns the `(slot, serial)` the
/// packet's private data must carry, or `None` (counted) for "no copy".
///
/// # Safety
/// Called from `DxgkDdiPresent` with its live argument struct; `src`/`dst` are
/// the fixed allocation-list slots decoded from it.
pub(crate) unsafe fn prepare(
    adapter: &AdapterContext,
    args: &DXGKARG_PRESENT,
    src: PresentAllocation,
    dst: PresentAllocation,
) -> Option<(u32, u32)> {
    // SAFETY: `DxgkDdiPresent` is `_IRQL_requires_(PASSIVE_LEVEL)`
    // (d3dkmddi.h), and this driver already writes the registry from it.
    let passive = unsafe { PassiveLevel::assume() };
    let result = unsafe { prepare_inner(passive, adapter, args, src, dst) };
    PC_COUNTERS.flush();
    result
}

enum VenusRefusal {
    NoObjects,
    Alias,
}

/// The KMD-device object a present allocation is copied through.
#[derive(Clone, Copy)]
enum Target {
    Image(u64),
    Buffer { id: u64, pitch: u32, offset: u64 },
}

unsafe fn prepare_inner(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    args: &DXGKARG_PRESENT,
    src: PresentAllocation,
    dst: PresentAllocation,
) -> Option<(u32, u32)> {
    let Some(src_alloc) = (unsafe { super::create_allocation::present_copy_allocation(src.handle()) })
    else {
        bump(&PC_SRC_REFUSED);
        return None;
    };
    let Some(dst_alloc) = (unsafe { super::create_allocation::present_copy_allocation(dst.handle()) })
    else {
        bump(&PC_DST_REFUSED);
        return None;
    };

    // Geometry: SrcRect/DstRect plus the destination clip list, bounded.
    let mut sub_rects = [CopyRect::new(0, 0, 0, 0); MAX_REGIONS];
    let sub_count = args.SubRectCnt as usize;
    let sub_rects: &[CopyRect] = if sub_count == 0 || args.pDstSubRects.is_null() {
        &[]
    } else if sub_count > MAX_REGIONS {
        bump(&PC_RECTS_FULL);
        &[]
    } else {
        for (i, out) in sub_rects.iter_mut().enumerate().take(sub_count) {
            // SAFETY: dxgkrnl supplies `SubRectCnt` RECTs at `pDstSubRects`
            // for the duration of the DDI; `i < SubRectCnt`.
            *out = rect(unsafe { &*args.pDstSubRects.add(i) });
        }
        &sub_rects[..sub_count]
    };
    let geometry = CopyGeometry {
        src_extent: (src_alloc.hwa2.width, src_alloc.hwa2.height),
        dst_extent: (dst_alloc.hwa2.width, dst_alloc.hwa2.height),
        src_rect: rect(&args.SrcRect),
        dst_rect: rect(&args.DstRect),
        sub_rects,
    };
    let mut regions = [CopyRegion {
        src_x: 0,
        src_y: 0,
        dst_x: 0,
        dst_y: 0,
        width: 0,
        height: 0,
    }; MAX_REGIONS];
    let region_count = match geometry.regions(&mut regions) {
        Ok(n) => n,
        Err(GeometryRefusal::Stretch) => {
            bump(&PC_STRETCH);
            return None;
        }
        Err(_) => {
            bump(&PC_GEOMETRY);
            return None;
        }
    };

    // One-shot census of the pair: `PcPair` = dst kind | dst std<<4 |
    // dst has KMD image<<8 | src swizzle<<12 | dst swizzle<<16.
    if PC_PAIR_LOGGED.swap(1, Ordering::Relaxed) == 0 {
        crate::diag::record_named_bytes(
            b"PcPair",
            (dst_alloc.hwa2.allocation_kind & 0xF)
                | ((dst_alloc.hwa2.standard_allocation_type & 0xF) << 4)
                | (u32::from(dst_alloc.venus_image_id != 0) << 8)
                | ((src_alloc.hwa2.swizzle_class & 0xF) << 12)
                | ((dst_alloc.hwa2.swizzle_class & 0xF) << 16),
        );
    }

    // Venus objects and both images, under the venus mutex.
    let venus = adapter.with_venus_client(passive, |client| {
        let ids = client
            .present_copy_ids(adapter)
            .map_err(|_| VenusRefusal::NoObjects)?;
        let src_image = resolve_alias(client, adapter, &src_alloc, 1)?;
        let dst_image = resolve_alias(client, adapter, &dst_alloc, 2)?;
        Ok::<_, VenusRefusal>((ids, src_image, dst_image))
    });
    let (ids, src_target, dst_target) = match venus {
        Ok(Ok(v)) => v,
        Ok(Err(VenusRefusal::NoObjects)) => {
            bump(&PC_NO_OBJECTS);
            return None;
        }
        Ok(Err(VenusRefusal::Alias)) => {
            bump(&PC_ALIAS_FAILED);
            return None;
        }
        Err(_) => {
            bump(&PC_NO_OBJECTS);
            return None;
        }
    };

    // Free completed/dead slots, then claim one.
    crate::virtio::ctrl::reap_parked(passive, adapter);
    reclaim(adapter);
    let (index, serial, held) = {
        let mut table = adapter.present_copy.slots.lock();
        let Some(index) = table
            .slots
            .iter()
            .position(|slot| slot.state == SlotState::Free)
        else {
            drop(table);
            bump(&PC_SLOT_FULL);
            return None;
        };
        let serial = table.next_serial.max(1);
        table.next_serial = serial.wrapping_add(1).max(1);
        let slot = &mut table.slots[index];
        slot.state = SlotState::Prepared { serial };
        (index, serial, slot.buffers.take())
    };

    // Encode and stage the stream: image→image, or image→pitched buffer for
    // a KMD staging-surface destination. A pitched SOURCE is not a present
    // shape this driver has seen and is refused by name.
    let Target::Image(src_image) = src_target else {
        release_slot(adapter, index, serial, held);
        crate::diag::record_named_bytes(b"PcAliasWhy", (1 << 8) | 8);
        bump(&PC_ALIAS_FAILED);
        return None;
    };
    let command_buffer = ids.command_buffers[index];
    let stream = match dst_target {
        Target::Image(dst_image) => logic::encode_copy_stream(
            CopyIds {
                queue: ids.queue,
                command_buffer,
                src_image,
                dst_image,
            },
            &regions[..region_count],
        ),
        Target::Buffer { id, pitch, offset } => logic::encode_copy_to_buffer_stream(
            CopyToBufferIds {
                queue: ids.queue,
                command_buffer,
                src_image,
                dst_buffer: id,
            },
            &regions[..region_count],
            pitch,
            offset,
        ),
    };
    let Some(bytes) = stream.finished() else {
        release_slot(adapter, index, serial, held);
        bump(&PC_ENCODE_FAILED);
        return None;
    };
    let Some((mut meta, mut venus_buffer)) = stage_buffers(passive, adapter, held, bytes.len())
    else {
        release_slot(adapter, index, serial, None);
        bump(&PC_BUFFER_FAILED);
        return None;
    };
    venus_buffer.as_mut_slice()[..bytes.len()].copy_from_slice(bytes);
    meta.as_mut_slice().fill(0);
    {
        let mut table = adapter.present_copy.slots.lock();
        let slot = &mut table.slots[index];
        if slot.state != (SlotState::Prepared { serial }) {
            // Reclaimed underneath us: impossible within one Present, but a
            // stale-serial reclaim rule exists, so keep the buffers parked.
            slot.buffers = Some((meta, venus_buffer));
            drop(table);
            bump(&PC_SLOT_FULL);
            return None;
        }
        slot.buffers = Some((meta, venus_buffer));
        slot.stream_len = bytes.len();
        slot.ring_idx = ids.ring_idx;
    }
    bump(&PC_PREPARED);
    Some((index as u32, serial))
}

/// The KMD-device object to name for this allocation: the KMD's own image for
/// a GDI-texture standard allocation, else an alias created once and cached —
/// an identical OPTIMAL image over a UMD texture, or a `VkBuffer` over a
/// pitched standard surface ([`TargetClass`]).
fn resolve_alias(
    client: &mut crate::virtio::venus::VenusClient,
    adapter: &AdapterContext,
    allocation: &PresentCopyAllocation,
    side: u32,
) -> Result<Target, VenusRefusal> {
    if allocation.venus_image_id != 0 {
        return Ok(Target::Image(allocation.venus_image_id));
    }
    let class = logic::target_class(&allocation.hwa2);
    let as_target = |id: u64| match class {
        Ok(TargetClass::LinearBuffer { pitch, offset }) => Target::Buffer { id, pitch, offset },
        _ => Target::Image(id),
    };
    match allocation.alias.load(Ordering::Acquire) {
        0 => {}
        PRESENT_ALIAS_REFUSED => return Err(VenusRefusal::Alias),
        id => return Ok(as_target(id)),
    }
    // First refusal per allocation only (the sentinel short-circuits after).
    // `PcAliasWhy` = side (1 src, 2 dst) << 8 | reason; `PcAliasDesc` = the
    // refused descriptor's kind | std<<4 | swizzle<<8 | bind<<12.
    let refuse = |reason: u32| {
        allocation
            .alias
            .store(PRESENT_ALIAS_REFUSED, Ordering::Release);
        crate::diag::record_named_bytes(b"PcAliasWhy", (side << 8) | reason);
        crate::diag::record_named_bytes(
            b"PcAliasDesc",
            (allocation.hwa2.allocation_kind & 0xF)
                | ((allocation.hwa2.standard_allocation_type & 0xF) << 4)
                | ((allocation.hwa2.swizzle_class & 0xF) << 8)
                | ((allocation.hwa2.bind_flags & 0xFFFF) << 12),
        );
        Err(VenusRefusal::Alias)
    };
    if allocation.venus_memory_id == 0 {
        return refuse(5);
    }
    let created = match class {
        Err(_) => return refuse(7),
        Ok(TargetClass::LinearBuffer { .. }) => client.create_present_alias_buffer(
            adapter,
            allocation.hwa2.byte_size,
            allocation.venus_memory_id,
            allocation.venus_alloc_size,
        ),
        Ok(TargetClass::OptimalImage) => {
            let spec = match logic::alias_image_spec(&allocation.hwa2) {
                Ok(spec) => spec,
                Err(reason) => return refuse(reason as u32),
            };
            client.create_present_alias_image(
                adapter,
                &spec,
                allocation.venus_memory_id,
                allocation.venus_alloc_size,
            )
        }
    };
    match created {
        Ok(id) => {
            allocation.alias.store(id, Ordering::Release);
            bump(&PC_ALIAS_CREATED);
            Ok(as_target(id))
        }
        Err(_) => refuse(6),
    }
}

/// `(meta, venus)` buffers for one stream: the slot's parked pair if it fits,
/// else the transport pool, else a fresh contiguous allocation (PASSIVE).
fn stage_buffers(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    held: Option<(DmaBuffer, DmaBuffer)>,
    stream_len: usize,
) -> Option<(DmaBuffer, DmaBuffer)> {
    if let Some((mut meta, mut venus)) = held {
        if meta.reset(SUBMIT_META_BYTES) && venus.reset(stream_len) {
            return Some((meta, venus));
        }
        let excess = adapter
            .with_virtio(|gpu| gpu.recycle_dma_buffers(alloc::vec![meta, venus]))
            .unwrap_or_default();
        drop(excess); // PASSIVE
    }
    let meta = adapter
        .with_virtio(|gpu| gpu.take_dma_buffer(SUBMIT_META_BYTES))
        .ok()
        .flatten()
        .or_else(|| DmaBuffer::new(passive, SUBMIT_META_BYTES))?;
    let venus = adapter
        .with_virtio(|gpu| gpu.take_dma_buffer(stream_len))
        .ok()
        .flatten()
        .or_else(|| DmaBuffer::new(passive, stream_len));
    match venus {
        Some(venus) => Some((meta, venus)),
        None => {
            let excess = adapter
                .with_virtio(|gpu| gpu.recycle_dma_buffers(alloc::vec![meta]))
                .unwrap_or_default();
            drop(excess);
            None
        }
    }
}

fn release_slot(
    adapter: &AdapterContext,
    index: usize,
    serial: u32,
    buffers: Option<(DmaBuffer, DmaBuffer)>,
) {
    let mut table = adapter.present_copy.slots.lock();
    let slot = &mut table.slots[index];
    if slot.state == (SlotState::Prepared { serial }) {
        slot.state = SlotState::Free;
    }
    if buffers.is_some() {
        slot.buffers = buffers;
    }
}

/// Free slots whose copy retired, whose ticket died with its engine
/// generation, or that were prepared and never submitted. PASSIVE.
fn reclaim(adapter: &AdapterContext) {
    // Issued slots normally free themselves in `on_copy_complete`; a terminal
    // that never arrived (full list, transport reset) is caught here so the
    // ticket is not left awaiting a host that already answered.
    let mut issued = [(0usize, 0u32, 0u64, None::<OrderedEngineTicket>); MAX_SLOTS];
    let mut issued_count = 0;
    {
        let table = adapter.present_copy.slots.lock();
        for (index, slot) in table.slots.iter().enumerate() {
            if let SlotState::Issued {
                serial,
                fence_id,
                ticket,
            } = slot.state
            {
                issued[issued_count] = (index, serial, fence_id, Some(ticket));
                issued_count += 1;
            }
        }
    }
    for &(index, serial, fence_id, ticket) in &issued[..issued_count] {
        let inflight = adapter
            .with_virtio(|gpu| gpu.async_fence_inflight(fence_id))
            .unwrap_or(false);
        if inflight {
            continue;
        }
        adapter.with_wddm_notify_lock(|guard| {
            let mut table = adapter.present_copy.slots.lock();
            let slot = &mut table.slots[index];
            if !matches!(slot.state, SlotState::Issued { serial: s, .. } if s == serial) {
                return;
            }
            slot.state = SlotState::Free;
            drop(table);
            bump(&PC_RECLAIMED);
            if let Some(ticket) = ticket {
                let _ = guard.mark_ordered_engine_host_completed(ticket);
            }
        });
    }
    adapter.with_wddm_notify_lock(|guard| {
        let mut table = adapter.present_copy.slots.lock();
        let now = table.next_serial;
        for slot in table.slots.iter_mut() {
            let dead = match slot.state {
                SlotState::Submitted { ticket, .. } => !guard.ordered_engine_ticket_is_live(ticket),
                // A Present that never reached SubmitCommand (the app died
                // in between) — older than every slot's worth of newer ones.
                SlotState::Prepared { serial } => now.wrapping_sub(serial) > 2 * MAX_SLOTS as u32,
                _ => false,
            };
            if dead {
                slot.state = SlotState::Free;
                bump(&PC_RECLAIMED);
            }
        }
    });
}

// ── phase 2: attach (DISPATCH, DxgkDdiSubmitCommand) ────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Attach {
    /// The ticket completes later, on the copy's wire fence.
    Deferred,
    /// No copy for this packet: complete the ticket now.
    CompleteNow,
}

pub(crate) fn attach(
    adapter: &AdapterContext,
    slot_index: u32,
    serial: u32,
    ticket: OrderedEngineTicket,
    resubmission: bool,
) -> Attach {
    let mut table = adapter.present_copy.slots.lock();
    let Some(slot) = table.slots.get_mut(slot_index as usize) else {
        drop(table);
        bump(&PC_ATTACH_MISS);
        return Attach::CompleteNow;
    };
    match slot.state {
        SlotState::Prepared { serial: s } if s == serial => {
            slot.state = SlotState::Submitted { serial, ticket };
            Attach::Deferred
        }
        SlotState::Submitted { serial: s, .. } if s == serial && resubmission => {
            slot.state = SlotState::Submitted { serial, ticket };
            bump(&PC_RESUBMITTED);
            Attach::Deferred
        }
        // Preempted after the copy was enqueued: the replay's ticket is the
        // one the copy's fence will complete. A copy that already finished
        // freed the slot, so the replay falls to `CompleteNow` below.
        SlotState::Issued {
            serial: s,
            fence_id,
            ..
        } if s == serial && resubmission => {
            slot.state = SlotState::Issued {
                serial,
                fence_id,
                ticket,
            };
            bump(&PC_RESUBMITTED);
            Attach::Deferred
        }
        _ => {
            drop(table);
            bump(&PC_ATTACH_MISS);
            Attach::CompleteNow
        }
    }
}

/// A present copy's wire fence retired: complete its ticket and free the slot.
/// Under the WDDM notify lock (the DPC's completion scope).
pub(crate) fn on_copy_complete(adapter: &AdapterContext, guard: &WddmNotifyGuard<'_>, fence: u64) {
    let ticket = {
        let mut table = adapter.present_copy.slots.lock();
        let Some(slot) = table.slots.iter_mut().find(
            |slot| matches!(slot.state, SlotState::Issued { fence_id, .. } if fence_id == fence),
        ) else {
            return;
        };
        let SlotState::Issued { ticket, .. } = slot.state else {
            return;
        };
        slot.state = SlotState::Free;
        ticket
    };
    bump(&PC_DONE);
    let _ = guard.mark_ordered_engine_host_completed(ticket);
}

// ── phase 3: issue (DISPATCH, under the WDDM notify lock) ───────────────────

/// If the K9 head is a submitted copy, enqueue it now. Returns `true` when the
/// head was marked host-complete here (immediately terminal, or refused and
/// completed without a copy), so the caller drains again.
pub(crate) fn issue_at_head(adapter: &AdapterContext, guard: &WddmNotifyGuard<'_>) -> bool {
    let Some(head) = guard.ordered_engine_head_awaiting_host() else {
        return false;
    };
    let (index, serial, buffers, stream_len, ring_idx) = {
        let mut table = adapter.present_copy.slots.lock();
        let Some((index, slot)) = table.slots.iter_mut().enumerate().find(|(_, slot)| {
            matches!(slot.state, SlotState::Submitted { ticket, .. } if ticket == head)
        }) else {
            return false;
        };
        let SlotState::Submitted { serial, .. } = slot.state else {
            return false;
        };
        (
            index,
            serial,
            slot.buffers.take(),
            slot.stream_len,
            slot.ring_idx,
        )
    };
    let Some(buffers) = buffers else {
        finish_without_copy(adapter, guard, head, index, None);
        bump(&PC_BUFFER_FAILED);
        return true;
    };

    let ctx_id = adapter.venus_ctx_id();
    // Moved into the closure only if it runs: a transport that is not started
    // returns `Err` without calling it, and the pair must come back to PASSIVE
    // custody rather than drop at DISPATCH.
    let mut pending = Some(buffers);
    let outcome = guard.with_virtio(|_order, gpu| {
        let Some((meta, venus)) = pending.take() else {
            return Err(None);
        };
        match gpu.enqueue_kmd_copy(ctx_id, ring_idx, meta, venus, stream_len) {
            Ok(fence_id) => Ok(fence_id),
            Err((meta, venus, _)) => Err(Some((meta, venus))),
        }
    });
    match outcome {
        Ok(Ok(fence_id)) => {
            let mut table = adapter.present_copy.slots.lock();
            table.slots[index].state = SlotState::Issued {
                serial,
                fence_id,
                ticket: head,
            };
            drop(table);
            bump(&PC_ISSUED);
            // Completes in `on_copy_complete` when the fence retires.
            false
        }
        Ok(Err(returned)) => {
            bump(&PC_ENQUEUE_FAILED);
            finish_without_copy(adapter, guard, head, index, returned);
            true
        }
        Err(_) => {
            bump(&PC_ENQUEUE_FAILED);
            finish_without_copy(adapter, guard, head, index, pending.take());
            true
        }
    }
}

/// Complete the head ticket with no copy (a black frame, counted) and park any
/// returned buffers in the slot for reuse at PASSIVE.
fn finish_without_copy(
    adapter: &AdapterContext,
    guard: &WddmNotifyGuard<'_>,
    head: OrderedEngineTicket,
    index: usize,
    buffers: Option<(DmaBuffer, DmaBuffer)>,
) {
    {
        let mut table = adapter.present_copy.slots.lock();
        let slot = &mut table.slots[index];
        slot.state = SlotState::Free;
        if buffers.is_some() {
            slot.buffers = buffers;
        }
    }
    bump(&PC_FALLBACK);
    let _ = guard.mark_ordered_engine_host_completed(head);
}
