//! Adapter context — one per virtio-gpu device the driver binds to.
//!
//! Allocated in `DxgkDdiAddDevice`, populated in `DxgkDdiStartDevice`, freed in
//! `DxgkDdiRemoveDevice`. Dxgkrnl hands this back to us as the opaque
//! `MiniportDeviceContext` in every subsequent DDI call.

use alloc::boxed::Box;
use alloc::sync::Arc;

use core::cell::UnsafeCell;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

use wdk_sys::ntddk::{KeAcquireSpinLockRaiseToDpc, KeReleaseSpinLock, KeSetEvent};
use wdk_sys::{KDPC, KEVENT, KSPIN_LOCK, KTIMER};

use crate::dxgk::*;
use crate::error::NotStarted;
use crate::virtio::{TransportOwner, TransportOwnerCreateError, VirtioGpu};
use helios_kmd_logic::DisplayMode;

pub(crate) mod allocation_object;
mod kobj;
mod locks;
mod segments;

pub(crate) use locks::{
    dump_ordered_engine_atomics, NotifyOrdered, OrderedEngineTicket, WddmNotifyGuard,
};
pub(crate) use segments::LocalSegment;

/// Move-only proof that this exact adapter has no installed transport.
pub(crate) struct TransportAbsent {
    adapter: usize,
    installable: bool,
}

impl TransportAbsent {
    pub(crate) const fn installable(&self) -> bool {
        self.installable
    }
}

/// Everything `DxgkDdiStartDevice` establishes, as one value published once.
///
/// StartDevice used to take a unique `&mut AdapterContext` that stayed live for
/// the whole function and mutated ~a dozen plain fields through it — while the
/// context pointer had been public to dxgkrnl since AddDevice and THREE
/// concurrent agents build `&AdapterContext` from it. `start_vsync` arms the
/// fixed-phase one-shot
/// timer and `init_hpd` starts a thread that both immediately take `&self` from
/// the same address while the outer `&mut` is still in scope, and
/// `install_virtio(gpu)` enables the device so the DIRQL ISR can fire
/// mid-function. That is an unambiguous Stacked-Borrows violation.
///
/// Split by LIFETIME, not by topic:
///
///   * the *sticky* half (everything outside `transport`) survives StopDevice.
///     This is load-bearing and must not be "tidied up": StopDevice today does
///     NOT clear the knobs, the mode or the EDID, and about two dozen sites
///     branch on `display_half`. Clearing them at StopDevice would flip all of
///     those from SUCCESS-shaped answers to NOT_SUPPORTED between StopDevice and
///     RemoveDevice, which is exactly what the restart-device leg of the
///     regression gate exercises.
///   * `transport`, which StopDevice clears, holds everything whose meaning dies
///     with the transport generation that produced it.
pub(crate) struct StartedState {
    /// Dxgkrnl callback interface, copied out of dxgkrnl's buffer at
    /// StartDevice. Read lock-free by the ISR and both DPCs; publishing it as
    /// part of this struct is what makes that visibility structural.
    pub dxgkrnl: Box<DXGKRNL_INTERFACE>,
    /// The WDDM 3.1+ shared-backing feature was explicitly admitted by dxgkrnl
    /// for this StartDevice generation.
    pub share_backing_store_with_kmd: bool,
    /// Every service-key knob, snapshotted once at StartDevice. See
    /// [`AdapterKnobs`].
    pub knobs: AdapterKnobs,
    /// The segment table this adapter REPORTS, built once in StartDevice from
    /// the same `LocalSegment` value every other consumer reads.
    ///
    /// `query_segments` renders this; it does not re-derive one. That is what
    /// makes the count reported on the descriptor-NULL call and the descriptors
    /// written on the second call the same immutable value — the premise the old
    /// SAFETY comment stated as its conclusion.
    pub segment_table: crate::ddi::segment_table::SegmentTable,
    /// Scanout-0 mode the display half presents, together with the EDID
    /// generated from it. See [`ScanoutMode`].
    pub scanout_mode: ScanoutMode,
    /// The half StopDevice clears. `None` between StopDevice and the next
    /// StartDevice.
    transport: UnsafeCell<Option<TransportGeneration>>,
}

/// The three surviving service-key policies, snapshotted once per StartDevice.
/// Segment shape and capacity are deliberately absent: K8 derives them only
/// from the active WDDM surface and exact transport capability.

#[derive(Clone, Copy)]
pub(crate) struct AdapterKnobs {
    /// `AllocCached` (default 1). When set, CpuVisible allocations are
    /// additionally flagged `Cached` so dxgkrnl maps CPU views write-back
    /// instead of write-combined. The BAR window is RAM-backed host shmem (x86
    /// cache-coherent for all agents on the same physical pages); WC reads
    /// measured ~200 MB/s in the IDD readback (36 ms per 7.8 MiB frame,
    /// 2026-07-06). 0 = kill switch.
    pub alloc_cached: bool,
    /// `DisplayHalf` (default 1 = ON — the render+display miniport IS the
    /// product; the hardware-accelerated desktop shipped on it). When nonzero,
    /// StartDevice advertises ONE video-present source + ONE child
    /// video-output and the VidPn/child DDIs in
    /// `ddi::display`/`ddi::vidpn`/`ddi::child` stand up a real virtual
    /// VidPn output + default monitor and drive virtio-gpu scanout, instead of
    /// returning NOT_SUPPORTED. 0 restores the boot-era render-only surface as
    /// the escape hatch (it was the default until 0ab-C closed and the display
    /// half became unconditional production). Demoted to 0 before publication
    /// if the transport never came up. Mirrored to `DspH`.
    pub display_half: bool,
    /// `CrossAdaptCaps` (default 0). Nonzero advertises
    /// `DXGK_VIDMMCAPS.CrossAdapterResource` (tier-1 cross-adapter copy support).
    /// The compile-time `DECLARE_CROSS_ADAPTER_RESOURCE` this used to be OR'd
    /// with was a `const false` and went with T6/R905; this knob is now the only
    /// way to set the bit.
    pub cross_adapter: bool,
}

impl AdapterKnobs {
    /// The value each knob takes when absent from the service key.
    ///
    /// Load-bearing, and it must not drift: with every knob absent this has to
    /// equal what [`Self::read`] produces, which is what makes the emitted
    /// descriptors byte-identical to the old per-writer `read_config_dword`
    /// calls. Kept as a named const because it is also the documented answer to
    /// "what does this driver do with no registry configuration at all".
    #[allow(dead_code)]
    pub const DEFAULTS: Self = Self {
        alloc_cached: true,
        display_half: true,
        cross_adapter: false,
    };

    /// Read every knob once. PASSIVE_LEVEL.
    ///
    /// Called twice per device lifetime: at AddAdapter (so the AddAdapter-time
    /// caps and segment queries answer from one immutable snapshot) and again in
    /// StartDevice through [`Self::read_at_start`], so `reg add` +
    /// `pnputil /restart-device` still picks up a change with no reboot.
    pub fn read() -> Self {
        use crate::diag::{knobs, read_config_dword};
        Self {
            alloc_cached: read_config_dword(knobs::ALLOC_CACHED, 1) != 0,
            display_half: read_config_dword(knobs::DISPLAY_HALF, 1) != 0,
            cross_adapter: read_config_dword(knobs::CROSS_ADAPT_CAPS, 0) != 0,
        }
    }

    /// [`Self::read`] plus the two existing policy breadcrumbs consumed by the
    /// bounded diagnostics path.
    pub fn read_at_start() -> Self {
        let knobs = Self::read();
        crate::diag::record_named_bytes(b"AlcC", knobs.alloc_cached as u32);
        crate::diag::record_named_bytes(b"DspH", knobs.display_half as u32);
        knobs
    }
}

impl StartedState {
    /// Copy only the callback bytes dxgkrnl says are present. The minimum ends
    /// immediately after `DxgkCbQueryFeatureSupport`, the last callback this
    /// package reads; a smaller table cannot support the advertised surface.
    #[inline(never)]
    pub(crate) unsafe fn copy_dxgkrnl_interface(
        source: *const DXGKRNL_INTERFACE,
    ) -> Option<Box<DXGKRNL_INTERFACE>> {
        const REQUIRED: usize = core::mem::offset_of!(DXGKRNL_INTERFACE, DxgkCbQueryFeatureSupport)
            + core::mem::size_of::<DXGKCB_QUERYFEATURESUPPORT>();

        // SAFETY: StartDevice supplies at least the interface prefix containing
        // Size. No other field is read until Size proves its coverage.
        let supplied = unsafe { (*source).Size as usize };
        if supplied < REQUIRED {
            return None;
        }
        let mut copy = Box::<DXGKRNL_INTERFACE>::new_uninit();
        let bytes = supplied.min(core::mem::size_of::<DXGKRNL_INTERFACE>());
        // SAFETY: zeroing the complete heap destination makes absent future
        // fields NULL; the source copy is bounded by both Size and our layout.
        unsafe {
            core::ptr::write_bytes(
                copy.as_mut_ptr() as *mut u8,
                0,
                core::mem::size_of::<DXGKRNL_INTERFACE>(),
            );
            core::ptr::copy_nonoverlapping(
                source as *const u8,
                copy.as_mut_ptr() as *mut u8,
                bytes,
            );
            Some(copy.assume_init())
        }
    }

    /// Build the sticky half DIRECTLY ON THE HEAP.
    ///
    /// ⚠ STACK BUDGET — this is not a style preference. `DxgkDdiStartDevice`
    /// calls `VirtioGpu::init`, whose frame is 3.0 KB even after the queue/error
    /// reductions, and the x64 kernel stack is 24 KB total. The historical
    /// embedded 576-byte interface copy took the nested boot chain to ~18.8 KB
    /// and caused the measured `0xc0000001` cold-boot failure. Both the bounded
    /// interface copy and this state are therefore constructed on the heap.
    /// That overflowed the kernel stack during boot, where dxgkrnl's own frames
    /// above us are deeper than on a live `devcon` restart: an early double
    /// fault with no dump, presenting as `0xc0000001` at the recovery screen.
    ///
    /// `#[inline(never)]` is load-bearing: it keeps the temporary in THIS
    /// frame, which is transient and does not overlap `VirtioGpu::init`.
    /// Callers must never bind the value — only the `Box`.
    ///
    /// The transport half always starts empty and is installed separately by
    /// [`AdapterContext::set_transport_generation`], so "a state published with a
    /// stale transport generation" is unrepresentable.
    ///
    /// # Safety
    /// StartDevice must remain the serialized publisher for this generation.
    #[inline(never)]
    pub(crate) unsafe fn boxed(
        dxgkrnl: Box<DXGKRNL_INTERFACE>,
        share_backing_store_with_kmd: bool,
        knobs: AdapterKnobs,
        scanout_mode: ScanoutMode,
        segment_table: crate::ddi::segment_table::SegmentTable,
    ) -> Box<Self> {
        Box::new(Self {
            dxgkrnl,
            share_backing_store_with_kmd,
            knobs,
            scanout_mode,
            segment_table,
            transport: UnsafeCell::new(None),
        })
    }
}

/// The scan-out mode the display half presents, and the EDID that describes it.
///
/// The two used to be three independent fields — `display_w`, `display_h` and a
/// 128-byte array — whose mutual consistency ("every VidPn mode + the generated
/// EDID derive from this so they stay cofunctional") was a comment. A future
/// write to the extent without regenerating the EDID would produce a monitor
/// whose detailed timing disagrees with the modes the VidPn DDIs enumerate:
/// the mismatch class that produced the mode-set retry loops.
///
/// The only constructor generates the EDID from the mode, so there is no way to
/// obtain a `ScanoutMode` whose EDID disagrees with its extent.
#[derive(Clone, Copy)]
pub(crate) struct ScanoutMode {
    mode: DisplayMode,
    edid: [u8; 128],
}

impl ScanoutMode {
    /// Adopt the host's extent if usable, else the 1920×1080 fallback, and
    /// generate the matching EDID.
    ///
    /// `host` is `VirtioGpu::display_mode`'s answer — note that method and
    /// `AdapterContext::display_mode` are different methods with the same name;
    /// only the latter reads this value.
    pub(crate) fn adopt(host: Option<(u32, u32)>) -> Self {
        let mode = host
            .and_then(|(w, h)| DisplayMode::from_host(w, h))
            .unwrap_or(DEFAULT_SCANOUT_EXTENT);
        Self {
            mode,
            edid: crate::ddi::vidpn::build_edid(mode.width(), mode.height()),
        }
    }

    /// A zeroed-EDID mode for the render-only surface, where no monitor is
    /// advertised and `QueryDeviceDescriptor` answers NOT_SUPPORTED.
    pub(crate) fn render_only() -> Self {
        Self {
            mode: DEFAULT_SCANOUT_EXTENT,
            edid: [0u8; 128],
        }
    }

    pub(crate) fn edid(&self) -> &[u8; 128] {
        &self.edid
    }

    pub(crate) fn extent(&self) -> (u32, u32) {
        (self.mode.width(), self.mode.height())
    }
}

/// The 1920×1080 fallback as an already-validated value, so nothing on the
/// display path needs an `unwrap` on a constant that cannot fail.
///
/// `DisplayMode::FALLBACK` is a total const expression with no panic path — see
/// its doc — and `kmd_logic`'s host tests pin it to the documented extent and to
/// `vidpn::DEFAULT_MODE_*`.
const DEFAULT_SCANOUT_EXTENT: DisplayMode = DisplayMode::FALLBACK;

/// The two fallback constants must not drift apart.
const _: () = {
    assert!(crate::ddi::vidpn::DEFAULT_MODE_WIDTH == helios_kmd_logic::FALLBACK_DISPLAY_WIDTH);
    assert!(crate::ddi::vidpn::DEFAULT_MODE_HEIGHT == helios_kmd_logic::FALLBACK_DISPLAY_HEIGHT);
};

/// State whose meaning dies with the transport generation that produced it.
///
/// Every resource id in here is meaningless in the next generation, whose ids
/// restart at 1 and whose liveness test is bare membership — which is how a
/// recycled id used to be accepted as the cached LINEAR scan-out target.
pub(crate) struct TransportGeneration {
    /// Exact optional local VidMm capacity for this transport. It has no CPU
    /// mapping address and is absent when the transport exposes no admissible
    /// host-visible capability.
    pub local_segment: Option<LocalSegment>,
    /// The persistent venus 3D context id (`VIRTIO_GPU_CAPSET_VENUS`) the venus
    /// client rides, created in StartDevice and destroyed in StopDevice. `0` = none.
    pub venus_ctx_id: u32,
}

pub struct AdapterContext {
    /// AddDevice's exact PDO, retained only so each HQA1 context can allocate
    /// one IoWorkItem for PASSIVE HOB1 snapshot/validation.  The work item is
    /// context-owned and joined before context teardown; this is not an
    /// adapter work queue or an execution identity.
    physical_device_object: PDEVICE_OBJECT,
    /// Service-key knobs as read at **AddAdapter**, for the window before
    /// StartDevice has published its own snapshot.
    ///
    /// That window is not hypothetical: dxgkrnl issues the DRIVERCAPS and
    /// QUERYSEGMENT queries during AddAdapter, so this is what those answer
    /// from. Written once in [`Self::new`], before the context is published, and
    /// never mutated — which is why it needs no `UnsafeCell` and no ordering,
    /// unlike everything below it. Reads go through [`Self::knobs`].
    initial_knobs: AdapterKnobs,
    /// `NbSegment` as reported on the descriptor-NULL call of the two-call
    /// QUERYSEGMENT protocol — the number of descriptor slots dxgkrnl then
    /// allocates. The descriptor pass clamps to this, because only dxgkrnl knows
    /// how many slots it allocated and this is the number we told it.
    reported_segment_count: AtomicU32,
    /// Everything StartDevice establishes, published ONCE. See [`StartedState`].
    ///
    /// Reached only through [`Self::started`]; there is no `&mut` path to it, so
    /// "mutate a started field from a DDI" does not compile and "read `dxgkrnl`
    /// before StartDevice" does not compile either (there is no `StartedState` to
    /// borrow).
    /// BOXED. The state is 832 bytes and embeds a 576-byte `DXGKRNL_INTERFACE`;
    /// keeping it behind a pointer is what stops it — and its by-value
    /// construction temporaries — from landing on `DxgkDdiStartDevice`'s stack
    /// frame. See `StartedState::boxed`.
    started: UnsafeCell<Option<Box<StartedState>>>,
    /// Publication flag for `started`. A reader that observes 1 with Acquire has
    /// necessarily seen every store StartDevice made into the state — which makes
    /// the callback-table visibility ordering structural for ALL THREE readers,
    /// not just the ISR. Two of them (the device DPC and the VSync DPC) read the
    /// table with no `isr_status` guard at all and used to rest on statement
    /// order plus a comment.
    started_published: AtomicU32,
    /// Per-adapter ETW admission; its epoch survives stop/start so a stale
    /// pre-stop CAS cannot enter the replacement transport generation.
    pub(crate) etw_rundown: crate::ddi::diag_etw::EtwAdapterRundown,
    /// Atomic committed-mode publication read by D2 admission. It is
    /// adapter-stable across transport replacement and is reset/removed only at
    /// the explicit lifecycle barriers.
    pub(crate) committed_mode: crate::ddi::committed_mode::CommittedModeStorage,
    /// One stable source-0/plane-0 D2 lifetime owner. The display path reaches
    /// it only through the SURFACE-derived `KMD_D2_OWNER_ENABLED` predicate.
    pub(crate) direct_scanout: crate::ddi::direct_scanout::DirectScanoutRuntime,
    /// Per-adapter native-fence identity, admission, epoch, and population.
    pub(crate) native_fence: Arc<crate::ddi::native_fence::NativeFenceAdapterState>,
    /// K11's fixed completion rundown. It carries no session identity, queue,
    /// or timeline; reset/stop use it only to join SubmitCommand callbacks that
    /// already own an exact host-terminal fence before retiring the scheduler
    /// or transport epoch.
    pub(crate) k11_completion: crate::ddi::session_transport::K11CompletionRundown,
    /// Last one-engine scheduler frontier reported to dxgkrnl.
    last_completed_fence: AtomicU32,
    /// Serializes K9 frontier admission/retirement, DMA_COMPLETED notification,
    /// and the monotonic completed watermark.
    wddm_notify_lock: UnsafeCell<KSPIN_LOCK>,
    /// K9's fixed one-node/one-engine scheduler-order frontier. The 256 slots
    /// live in a separate heap allocation so AdapterContext construction does
    /// not materialize a multi-KiB array on the kernel boot stack. Mutation is
    /// reachable only through `WddmNotifyGuard`.
    ordered_engine: UnsafeCell<Box<locks::OrderedEngineFrontier>>,
    /// Successfully delivered DMA frontier edges still owed one K7 empty-array
    /// native-fence rescan. A saturating count preserves every delivered edge
    /// across a failed callback; the notification guard owns every mutation.
    ordered_engine_native_rescans: AtomicU32,
    /// Mapped kernel VA of the virtio ISR-status register (read-to-clear), or 0
    /// until StartDevice wires it. `DxgkDdiInterruptRoutine` reads this at DIRQL to
    /// acknowledge the level-triggered INTx line (the device is `MSISupported=0`);
    /// without it the line stays asserted → interrupt storm → Windows disables the
    /// adapter (Code 43). Set once in StartDevice, read lock-free in the ISR — an
    /// atomic (not behind `virtio_lock`) because the ISR runs at DIRQL and cannot
    /// take the spinlock.
    pub isr_status: AtomicUsize,
    /// Serializes ALL access to `virtio` (the control virtqueue + the shared
    /// scratch page). Held by escape submissions at PASSIVE_LEVEL and, from M3.4,
    /// by the used-ring DPC at DISPATCH_LEVEL — a spinlock (not a mutex) is
    /// mandatory because the DPC path cannot block. `0` is the initialized +
    /// unlocked state of a `KSPIN_LOCK`, so no explicit `KeInitializeSpinLock` is
    /// required (same rationale as the BAR-mapping cache in `virtio::hal`).
    virtio_lock: UnsafeCell<KSPIN_LOCK>,
    /// The heap-backed virtio-gpu transport, brought up in
    /// `DxgkDdiStartDevice` (Phase 2). The `Box` is load-bearing for the kernel
    /// boot-stack budget; see `VirtioGpu::init`.
    /// Guarded by `virtio_lock`; `None` until StartDevice (and after StopDevice).
    virtio: UnsafeCell<Option<Box<VirtioGpu>>>,
    /// Read-only DIRQL publication of the fixed interrupt-synchronized D4
    /// queue. Zero outside a fully installed owner-enabled transport.
    ///
    /// The pointee is the separately boxed `InterruptQueue` owned by the
    /// heap-backed `VirtioGpu`. Installation publishes it only after the GPU Box
    /// is in `virtio`; removal withdraws it before taking that owner Box out.
    d4_dirql_queue: AtomicUsize,
    /// Sequence-consistent hazard count for the raw D4 queue publication.
    /// The DIRQL publisher and PASSIVE physical reset both increment before
    /// rechecking the pointer; teardown first clears the pointer and may free
    /// the Box only after observing zero.
    d4_dirql_readers: AtomicU32,
    /// Sticky fail-closed latch. If teardown catches a live DIRQL reader it
    /// retains the transport and prevents both replacement and adapter free.
    d4_dirql_transport_retained: AtomicU32,
    transport_owner: TransportOwner,
    /// PASSIVE-level serialization for scanout selection versus allocation
    /// destruction. A Windows primary can be replaced while an asynchronous
    /// SET_SCANOUT_BLOB/RESOURCE_FLUSH is outstanding; destruction must first
    /// retire that exact resource from scanout 0 and drain the control queue
    /// before RESOURCE_UNREF. This event is an in-place synchronization mutex,
    /// separate from the DISPATCH-safe virtio spinlock because the protected
    /// operations may perform synchronous host round-trips.
    scanout_mutex: UnsafeCell<KEVENT>,
    /// Exact paging-process system-memory leaf PTEs supplied by VidMm for
    /// virtual content transfers. This is the software VA-walk state used by
    /// `DxgkDdiBuildPagingBuffer`, independent of the decorative hardware page
    /// tables that venus never reads.
    pub paging_pte_shadow: crate::ddi::PagingPteShadow,
    /// Exact system-memory pages Windows associates with a BAR allocation
    /// through paging TRANSFER requests.
    // ⛔ `vidmm_trackers: VidMmTrackerTable` lived here. It was the attestation
    // half of UMD-backing adoption: its only producer was the retired
    // `HELIOS_WDDM_ALLOC_KIND_TRACKING` create and its only consumer the
    // adopted-allocation one-page VidMm charge. HWA2 has no tracking kind, no
    // cookie, and no global-share field (`grep -in track protocol/src/wddm.rs`
    // is empty), so the mechanism has **no successor** — it is not "folded
    // into" anything (`docs/retirement/K4-CONTRACT.md` §6 corrects
    // `wddm_legacy.rs:30`, which said it was). §14:3421's "a raw resid or HPS
    // slot is never accepted as identity" forbids reintroducing it under
    // another name.
    /// The persistent venus client (ring/reply BAR mappings + Vulkan ids) kept
    /// alive for the device lifetime so the page-table blob stays mapped. `None`
    /// until/unless the StartDevice venus bring-up succeeds. Its `Drop` unmaps the
    /// ring/reply kernel mappings; cleared (dropped) in StopDevice. Guarded by
    /// [`Self::venus_mutex`] — a PASSIVE mutex, NOT the virtio spinlock: client
    /// operations block on host round-trips / ring progress (C3/M3.4) and must
    /// never hold a spinlock across those waits.
    venus_client: UnsafeCell<Option<crate::virtio::venus::VenusClient>>,
    /// PASSIVE mutex serializing `venus_client` access: a SynchronizationEvent
    /// (auto-clearing) that starts signaled. Initialized IN PLACE by
    /// [`Self::init_kernel_events`] after the context reaches its final heap
    /// address (a KEVENT's dispatcher header is self-referential once
    /// initialized — it must never be moved afterwards).
    venus_mutex: UnsafeCell<KEVENT>,
    /// VSync heartbeat timer for the display half. A fixed-phase 60 Hz one-shot
    /// prefers a system-allocated `EX_TIMER_HIGH_RESOLUTION` timer and falls
    /// back to this embedded `SynchronizationTimer`/DPC pair only if that
    /// allocation failed. Both sources synthesize
    /// `DXGK_INTERRUPT_CRTC_VSYNC` so dxgkrnl advances the flip queue and issues
    /// `SetVidPnSourceAddress` — the heartbeat a render-only adapter structurally
    /// lacks (the viogpu3d FlipThread analog, `viogpu_vidpn.cpp:1977`). Both are
    /// zeroed here and initialized in place by [`Self::init_kernel_events`]
    /// before publication (a KTIMER/KDPC is self-referential once initialized —
    /// never move it afterwards), then only armed/cancelled by display lifecycle
    /// paths. Only armed when `display_half`.
    pub vsync_timer: UnsafeCell<KTIMER>,
    pub vsync_dpc: UnsafeCell<KDPC>,
    /// `PEX_TIMER` returned by `ExAllocateTimer`, stored as an integer so the
    /// stable adapter context remains `Sync`. Zero means allocation failed and
    /// the embedded `KTIMER` fallback is active for this adapter's lifetime.
    /// The pointer is deleted exactly once from `Drop` at final RemoveDevice,
    /// after `stop_vsync` has cancelled it.
    pub vsync_ex_timer: AtomicUsize,
    /// Interrupt-time deadline (100 ns units) of the one-shot tick currently
    /// armed. Advancing this fixed phase avoids both the old 16 ms/62.5 Hz mode
    /// mismatch and cumulative DPC-latency drift.
    pub vsync_deadline_100ns: AtomicU64,
    /// CRTC_VSYNC delivery gate: default 1 once the display half arms the timer,
    /// toggled by `DxgkDdiControlInterrupt(DXGK_INTERRUPT_CRTC_VSYNC, enable)`.
    /// The DPC only synthesizes an interrupt while this is nonzero.
    pub vsync_enabled: AtomicU32,
    /// Count of CRTC_VSYNC interrupts synthesized this boot (diag `ScVs`).
    pub vsync_count: AtomicU32,
    /// Physical address of the last primary actually programmed for display,
    /// reported in each CRTC_VSYNC packet so dxgkrnl can retire the matching
    /// queued flip (viogpu3d `m_sourceAddress`). Direct scanout publishes only
    /// after SET_SCANOUT_BLOB succeeds; the copy fallback publishes from the
    /// ring-1 GPU-completion DPC. 0 until the first completed source switch.
    pub last_primary_address: AtomicU64,
    /// ── Wire-order guard for scan-out bind bookkeeping (defect 0ab-C, D1(ii)) ─
    ///
    /// Minted at every `SET_SCANOUT_BLOB` enqueue, INSIDE the same `with_virtio`
    /// as the enqueue itself — so the sequence is totally ordered consistently
    /// with the control queue's FIFO order, which is the order the host applies
    /// the binds in.
    ///
    /// It exists because the two enqueue sites apply their guest-side
    /// bookkeeping at completely different times: the DISPATCH fast bind applies
    /// from the used-ring drain, while the PASSIVE worker applies after its
    /// synchronous round-trip returns. A worker bind that started BEFORE a fast
    /// bind can therefore finish its bookkeeping AFTER it (the common case at
    /// 210 fps, where a flip arms mid-round-trip) and stomp the newer identity
    /// with an older one.
    pub scanout_bind_wire_seq: AtomicU64,
    /// Nonwrapping reservation high-water; committed wire history advances only
    /// after the matching descriptor was accepted by the queue.
    pub scanout_bind_next_seq: AtomicU64,
    /// Resource named by the newest descriptor accepted in this transport
    /// generation. Candidate custody remains in the fixed direct queue slot;
    /// this scalar is retained only with its committed sequence fact.
    pub scanout_bind_wire_resource: AtomicU32,
    /// 1 while the fixed-phase one-shot is armed (quiesce/StopDevice clear it).
    pub vsync_armed: AtomicU32,
    /// HPD worker event. `DxgkCbIndicateChildStatus` — which tells the OS the child
    /// video-output is *connected*, the transition that makes the target available
    /// for a VidPn path — is PASSIVE-only and MUST NOT be called during StartDevice,
    /// so a dedicated system thread ([`crate::ddi::hpd::hpd_thread_routine`], the
    /// viogpu3d ThreadWorkRoutine analog) does it. This SynchronizationEvent wakes
    /// that thread: once shortly after start, on virtio config changes, after a
    /// completion-ordered scanout marker, and after async RESOURCE_FLUSH completion.
    pub hpd_event: UnsafeCell<KEVENT>,
    /// Set by the HPD worker immediately before `PsTerminateSystemThread`, at
    /// BOTH of its exit sites. `stop_hpd` waits on this rather than depending on
    /// `ObReferenceObjectByHandle` succeeding: a NotificationEvent stays
    /// signalled, so the join cannot miss it and cannot be defeated by a failed
    /// handle-to-object lookup.
    pub hpd_exited: UnsafeCell<KEVENT>,
    /// PsCreateSystemThread handle for the HPD worker (0 = not started). StopDevice
    /// signals `hpd_stop` + `hpd_event`, joins the thread on this handle, then closes it.
    hpd_thread: AtomicUsize,
    /// Tells the HPD worker to terminate (StopDevice / teardown).
    pub hpd_stop: AtomicU32,
    /// Set when `stop_hpd` could NOT prove the worker exited (the thread-object
    /// reference failed, or the bounded join timed out). RemoveDevice must then
    /// leak this context rather than free memory a live worker still touches.
    pub hpd_worker_leaked: AtomicU32,
    /// Set by the ISR when the virtio config-change bit (ISR status bit 1) fires;
    /// the DPC signals `hpd_event`, then the PASSIVE worker consumes this bit and
    /// re-indicates connection.
    pub config_change_pending: AtomicU32,
    /// 1 once `DxgkDdiStartDevice` has returned.
    ///
    /// `DxgkCbIndicateChildStatus` is forbidden DURING StartDevice, and the HPD
    /// worker is a thread StartDevice itself spawned — so "StartDevice has
    /// returned" cannot be a compile-time fact. It used to be approximated by a
    /// 500 ms relative wait, i.e. a delay standing in for an event, with nothing
    /// actually observing the return. This is the real edge: StartDevice sets it
    /// and signals `hpd_event`, which demotes the timeout to a documented
    /// fallback rather than the mechanism.
    pub start_complete: AtomicU32,
}

// SAFETY (rewritten by R510 to name what is actually here now):
//
//   * `started` is an `UnsafeCell<Option<StartedState>>` written exactly once, by
//     StartDevice, and published with a Release store to `started_published`.
//     Every reader goes through `started()`, whose Acquire load pairs with it, so
//     no reader can observe a partially-built state. There is no `&mut` path to
//     it after publication — StartDevice no longer forms `&mut AdapterContext` at
//     all, which is what makes the ISR, both DPCs and the HPD worker building
//     `&AdapterContext` from the same pointer sound rather than a Stacked-Borrows
//     violation.
//   * `StartedState::transport` is interior-mutable and written only by
//     StartDevice and StopDevice, which Dxgkrnl serializes against each other and
//     against every DDI that reads it.
//   * `virtio` is interior-mutable but every access goes through `virtio_lock`
//     (a kernel spinlock) via `with_virtio`/`set_virtio`, so concurrent
//     escape/DPC callers never alias it.
//   * `venus_client` is guarded by the PASSIVE `venus_mutex`.
//
// This is the genuine lock-guarded state that replaces Phase-2's
// hand-asserted-without-a-lock Send/Sync.
unsafe impl Send for AdapterContext {}
unsafe impl Sync for AdapterContext {}

impl AdapterContext {
    /// Exact PDO supplied by dxgkrnl at AddDevice.  Outer HQA1 contexts use it
    /// only to allocate their own one-shot PASSIVE work item; it is never an
    /// allocation, session, or submission lookup key.
    pub(crate) fn physical_device_object(&self) -> PDEVICE_OBJECT {
        self.physical_device_object
    }

    /// Allocate a fully initialised adapter context and return only a pointer to
    /// it.
    ///
    /// This is the ONLY way to obtain an `AdapterContext`, and it deliberately
    /// never returns `Self`. `new` builds five self-referential kernel
    /// dispatcher objects as zeroed placeholders (`scanout_mutex`,
    /// `venus_mutex`, `vsync_timer`, `vsync_dpc`, `hpd_event`), so the real
    /// headers can only be written once the context is at its final heap
    /// address. As two public steps, this compiled:
    ///
    ///   let ctx = AdapterContext::new()?;
    ///   let boxed = Box::new(ctx);
    ///   /* no init_kernel_events */
    ///   *out = Box::into_raw(boxed);
    ///
    /// and produced an adapter whose first `with_venus_client` would
    /// `KeWaitForSingleObject` on an uninitialised KEVENT — typically an
    /// unrecoverable hang, with no diagnostic. Equally,
    /// `let moved = *Box::from_raw(raw);` after init moved initialised,
    /// self-referential dispatcher objects.
    ///
    /// Folding the two phases into one private constructor makes the skip
    /// unrepresentable for safe callers, and returning only `NonNull` — never
    /// `Self` — is what makes the no-move invariant hold: a caller who never
    /// obtains the value cannot move it. `PhantomPinned` is deliberately NOT
    /// used as the guarantee; `!Unpin` affects only the `Pin` APIs and would not
    /// stop `Box::new(ctx)` or `*Box::from_raw(raw)` from compiling.
    ///
    /// Domain or fixed owner-storage exhaustion is refused before allocating
    /// this context or initializing any dispatcher object.
    ///
    pub(crate) fn create(
        physical_device_object: PDEVICE_OBJECT,
    ) -> Result<NonNull<AdapterContext>, TransportOwnerCreateError> {
        let transport_owner = TransportOwner::unbound()?;
        let raw = Box::into_raw(Box::new(Self::new(transport_owner, physical_device_object)));
        // SAFETY: `Box::into_raw` never returns null.
        let context = unsafe { NonNull::new_unchecked(raw) };
        // SAFETY: `context` is this owner's final heap address, freshly allocated,
        // and no other thread can see it yet.
        unsafe { (*raw).bind_transport_owner(context) };
        // Kernel dispatcher objects must be initialized at the context's FINAL
        // address — a KEVENT's header is self-referential.
        // SAFETY: `raw` is the final heap address, freshly allocated, and no
        // other thread can see it yet.
        unsafe { (*raw).init_kernel_events() };
        Ok(context)
    }

    /// Private: an `AdapterContext` by value is only ever a transient inside
    /// [`Self::create`], before the in-place dispatcher init runs.
    fn new(transport_owner: TransportOwner, physical_device_object: PDEVICE_OBJECT) -> Self {
        Self {
            physical_device_object,
            // PASSIVE_LEVEL: `create` is called from AddAdapter. Read here so the
            // AddAdapter-time caps/segment queries — which run BEFORE
            // StartDevice — answer from the registry rather than from
            // `DEFAULTS`. Written once, before this context is published, so it
            // needs no interior mutability and no synchronisation.
            initial_knobs: AdapterKnobs::read(),
            reported_segment_count: AtomicU32::new(
                crate::ddi::segment_table::SegmentTable::MAX as u32,
            ),
            started: UnsafeCell::new(None),
            started_published: AtomicU32::new(0),
            etw_rundown: crate::ddi::diag_etw::EtwAdapterRundown::new(),
            committed_mode: crate::ddi::committed_mode::CommittedModeStorage::new(),
            direct_scanout: crate::ddi::direct_scanout::DirectScanoutRuntime::new(),
            native_fence: Arc::new(crate::ddi::native_fence::NativeFenceAdapterState::new()),
            k11_completion: crate::ddi::session_transport::K11CompletionRundown::new(),
            last_completed_fence: AtomicU32::new(0),
            wddm_notify_lock: UnsafeCell::new(0),
            ordered_engine: UnsafeCell::new(locks::allocate_ordered_engine_frontier()),
            ordered_engine_native_rescans: AtomicU32::new(0),
            isr_status: AtomicUsize::new(0),
            virtio_lock: UnsafeCell::new(0),
            virtio: UnsafeCell::new(None),
            d4_dirql_queue: AtomicUsize::new(0),
            d4_dirql_readers: AtomicU32::new(0),
            d4_dirql_transport_retained: AtomicU32::new(0),
            transport_owner,
            // Zeroed placeholder — initialized in place by init_kernel_events.
            scanout_mutex: UnsafeCell::new(unsafe { core::mem::zeroed() }),
            paging_pte_shadow: crate::ddi::PagingPteShadow::new(),
            venus_client: UnsafeCell::new(None),
            // Zeroed placeholder — the real dispatcher header is written by
            // `init_kernel_events` once the context is at its final address.
            venus_mutex: UnsafeCell::new(unsafe { core::mem::zeroed() }),
            // Zeroed placeholders — initialized by `init_kernel_events` once
            // the context reaches its final address, before publication.
            vsync_timer: UnsafeCell::new(unsafe { core::mem::zeroed() }),
            vsync_dpc: UnsafeCell::new(unsafe { core::mem::zeroed() }),
            vsync_ex_timer: AtomicUsize::new(0),
            vsync_deadline_100ns: AtomicU64::new(0),
            vsync_enabled: AtomicU32::new(0),
            vsync_count: AtomicU32::new(0),
            last_primary_address: AtomicU64::new(0),
            vsync_armed: AtomicU32::new(0),
            scanout_bind_wire_seq: AtomicU64::new(0),
            scanout_bind_next_seq: AtomicU64::new(0),
            scanout_bind_wire_resource: AtomicU32::new(0),
            // Zeroed placeholder — the real KEVENT is written by init_kernel_events.
            hpd_event: UnsafeCell::new(unsafe { core::mem::zeroed() }),
            hpd_exited: UnsafeCell::new(unsafe { core::mem::zeroed() }),
            hpd_thread: AtomicUsize::new(0),
            hpd_stop: AtomicU32::new(0),
            hpd_worker_leaked: AtomicU32::new(0),
            config_change_pending: AtomicU32::new(0),
            start_complete: AtomicU32::new(0),
        }
    }

    unsafe fn bind_transport_owner(&self, address: NonNull<Self>) {
        // SAFETY: forwarded only by `create` before dispatcher initialization or
        // publication, at the final `Box` address.
        unsafe { self.transport_owner.bind_adapter_once(address) };
    }

    pub(crate) fn reset_dormant_owner_transition_diagnostics(
        &self,
        passive: crate::irql::PassiveLevel,
    ) {
        self.transport_owner.reset_transition_diagnostics(passive);
    }

    /// Publish "StartDevice has returned" and wake the HPD worker.
    ///
    /// The last thing `DxgkDdiStartDevice` does. Before this, the worker's
    /// prologue simply waited 500 ms and hoped; now the wait has a real wake
    /// source and the timeout is only a fallback.
    pub(crate) fn signal_start_complete(&self) {
        self.start_complete.store(1, Ordering::Release);
        // SAFETY: hpd_event was initialized in place by init_kernel_events;
        // KeSetEvent(Wait=FALSE) is legal through DISPATCH_LEVEL.
        unsafe { KeSetEvent(self.hpd_event.get(), 0, 0) };
    }

    pub(crate) fn with_k11_completion<R>(&self, operation: impl FnOnce() -> R) -> Option<R> {
        self.k11_completion.with_admitted(operation)
    }

    pub(crate) fn close_k11_completions_and_wait(&self, passive: crate::irql::PassiveLevel) {
        self.k11_completion.close_completion_and_wait(passive);
        // Every current K9 callback source is inside this rundown. Once joined,
        // close and generation-invalidate the scheduler frontier under the one
        // notification lock before reset/stop can abandon transport state.
        self.with_wddm_notify_lock(|guard| guard.invalidate_ordered_engine());
    }

    pub(crate) fn reopen_k11_completions(&self) {
        // Publish the empty successor engine generation before admitting a
        // callback that could try to complete into it.
        let engine_open = self.with_wddm_notify_lock(|guard| guard.reopen_ordered_engine());
        if engine_open {
            self.k11_completion.reopen();
        }
    }

    /// Clear the generation-local CRTC publication before a successor transport
    /// can report display state. Direct-plane custody is reset separately by
    /// `direct_scanout` under its own lifetime contract.
    pub fn reset_display_publication_state(&self) {
        self.last_primary_address.store(0, Ordering::Release);
    }

    /// The display half's scanout-0 mode `(width, height)`: the host-reported size
    /// if usable, else the 1920×1080 fallback. Every VidPn mode + the generated
    /// EDID derive from this so they stay mutually consistent (cofunctional).
    /// A field read now: the minimum-size check ran ONCE, in
    /// [`ScanoutMode::adopt`], instead of re-running here on every call through
    /// bare literals. Returns the same `(u32, u32)` tuple as before, so its five
    /// consumers are untouched.
    pub fn display_mode(&self) -> (u32, u32) {
        self.started()
            .map_or(DEFAULT_SCANOUT_EXTENT, |s| s.scanout_mode.mode)
            .into()
    }

    /// The packed `(w << 16) | h` the `DspMd` breadcrumb reports.
    pub(crate) fn display_mode_packed(&self) -> u32 {
        self.started()
            .map_or(DEFAULT_SCANOUT_EXTENT, |s| s.scanout_mode.mode)
            .packed()
    }

    /// The state StartDevice established, or `None` before it ran.
    ///
    /// The `Acquire` pairs with the `Release` in [`Self::publish_started`], so a
    /// caller that gets `Some` has necessarily observed every field — including
    /// the multi-hundred-byte `DXGKRNL_INTERFACE` the ISR and both DPCs read
    /// lock-free.
    pub(crate) fn started(&self) -> Option<&StartedState> {
        if self.started_published.load(Ordering::Acquire) == 0 {
            return None;
        }
        // SAFETY: the slot is written exactly once, by `publish_started`, before
        // the flag above is set with Release. Observing the flag with Acquire
        // therefore happens-after that write, and nothing ever takes a `&mut` to
        // the slot afterwards — the transport half has its own interior
        // mutability and its own serialization (StartDevice/StopDevice, which
        // dxgkrnl serializes).
        unsafe { (*self.started.get()).as_deref() }
    }

    /// Publish the started state. StartDevice only, exactly once per start.
    ///
    /// # Safety
    /// Must be called at PASSIVE_LEVEL from `DxgkDdiStartDevice`, which dxgkrnl
    /// serializes against every other lifecycle DDI, and only while
    /// [`Self::started`] is `None` for this start.
    pub(crate) unsafe fn publish_started(&self, state: Box<StartedState>) {
        // Takes the Box, so only a pointer moves through this call — the 832-byte
        // value is never copied through a caller's frame.
        // SAFETY: per the fn contract — StartDevice is serialized, and no reader
        // can observe the slot until the Release store below.
        unsafe { *self.started.get() = Some(state) };
        self.started_published.store(1, Ordering::Release);
    }

    /// Borrow the Dxgkrnl interface, or fail if StartDevice has not run yet.
    pub fn dxgkrnl(&self) -> Result<&DXGKRNL_INTERFACE, NotStarted> {
        self.dxgkrnl_opt().ok_or(NotStarted)
    }

    /// The Dxgkrnl callback table, or `None` before StartDevice.
    ///
    /// Replaces the direct `adapter.dxgkrnl.as_ref()` field reads. Every caller
    /// now goes through the published slot, so the ordering that makes the table
    /// safe to read is the Acquire in [`Self::started`] rather than statement
    /// order plus a comment.
    pub fn dxgkrnl_opt(&self) -> Option<&DXGKRNL_INTERFACE> {
        self.started().map(|s| s.dxgkrnl.as_ref())
    }

    pub(crate) fn share_backing_store_with_kmd(&self) -> bool {
        self.started()
            .is_some_and(|s| s.share_backing_store_with_kmd)
    }

    /// The service-key knob snapshot, or [`AdapterKnobs::DEFAULTS`] before
    /// StartDevice has published one.
    ///
    /// Every knob reader in the driver goes through here, so "a query landing
    /// before StartDevice sees the documented defaults" is one branch in one
    /// place rather than a per-accessor `map_or` whose fallback could drift from
    /// the `read_config_dword` default it is supposed to mirror.
    ///
    /// Returned by value: `AdapterKnobs` is a small `Copy` POD, and handing out a
    /// reference would tie every caller's borrow to the published slot.
    pub(crate) fn knobs(&self) -> AdapterKnobs {
        self.started().map_or(self.initial_knobs, |s| s.knobs)
    }

    /// Latch the `NbSegment` reported on the descriptor-NULL call.
    ///
    /// `Relaxed` is sufficient and correct: both calls of the two-call protocol
    /// arrive on the same dxgkrnl-serialized PASSIVE path, so this is a plain
    /// carry between them, not a synchronisation edge.
    pub(crate) fn latch_reported_segment_count(&self, count: u32) {
        self.reported_segment_count
            .store(count, core::sync::atomic::Ordering::Relaxed);
    }

    /// The count latched by [`Self::latch_reported_segment_count`], i.e. the
    /// number of descriptor slots dxgkrnl allocated. `SegmentTable::MAX` before
    /// any NULL call, so a descriptor pass that somehow arrived first is bounded
    /// by the table's own capacity rather than by zero.
    pub(crate) fn reported_segment_count(&self) -> u32 {
        self.reported_segment_count
            .load(core::sync::atomic::Ordering::Relaxed)
    }

    /// The reported segment table, or `None` before StartDevice has published one.
    ///
    /// `None` is not reachable in practice — a DiagLevel=1 ring shows StartDevice
    /// completing before the first QueryAdapterInfo — but the query path handles
    /// it explicitly rather than assuming, and falls back to the aperture-only
    /// shape it would otherwise have emitted.
    pub(crate) fn segment_table(&self) -> Option<crate::ddi::segment_table::SegmentTable> {
        self.started().map(|s| s.segment_table)
    }

    /// `DisplayHalf`: whether the display DDIs answer for real. False before
    /// StartDevice and on the render-only recovery shape.
    pub fn display_half(&self) -> bool {
        self.knobs().display_half
    }

    /// `AllocCached`. Defaults to TRUE before StartDevice, matching the value the
    /// field was constructed with.
    pub fn alloc_cached(&self) -> bool {
        self.knobs().alloc_cached
    }

    /// The EDID served by `DxgkDdiQueryDeviceDescriptor`.
    ///
    /// Generated from — and therefore always consistent with — the extent
    /// `display_mode()` reports: they are one value.
    pub fn edid(&self) -> Option<&[u8; 128]> {
        self.started().map(|s| s.scanout_mode.edid())
    }

    /// The current transport generation's state, or `None` between StopDevice
    /// and the next StartDevice.
    pub(crate) fn transport_generation(&self) -> Option<&TransportGeneration> {
        // SAFETY: the cell is written only by StartDevice and StopDevice, which
        // dxgkrnl serializes against each other and against every DDI that could
        // read it. The reference borrows `self`, so it cannot outlive the
        // adapter.
        self.started()
            .and_then(|s| unsafe { (*s.transport.get()).as_ref() })
    }

    /// Install the transport generation. StartDevice only.
    ///
    /// # Safety
    /// PASSIVE_LEVEL, from `DxgkDdiStartDevice`, which dxgkrnl serializes.
    pub(crate) unsafe fn set_transport_generation(&self, generation: Option<TransportGeneration>) {
        let Some(state) = self.started() else {
            return;
        };
        // SAFETY: per the fn contract.
        unsafe { *state.transport.get() = generation };
    }

    /// Replace only the reset-local Venus context after a verified TDR reset.
    /// The PCI BAR and reported segment table survive that reset unchanged.
    ///
    /// # Safety
    /// ResetFromTimeout/RestartFromTimeout serialize this write with transport
    /// teardown and no control producer may run until the replacement context
    /// has been published.
    pub(crate) unsafe fn set_reset_venus_context(&self, context_id: u32) -> bool {
        let Some(state) = self.started() else {
            return false;
        };
        let Some(generation) = (unsafe { &mut *state.transport.get() }).as_mut() else {
            return false;
        };
        generation.venus_ctx_id = context_id;
        true
    }

    /// The venus 3D context id for this transport generation, or 0.
    pub fn venus_ctx_id(&self) -> u32 {
        self.transport_generation().map_or(0, |t| t.venus_ctx_id)
    }

    /// The local-memory segment for this transport generation, if any.
    /// The reported segment that is a CPU host aperture, if this adapter has
    /// one. Today that is the local-memory segment; the aperture (id 1) holds
    /// no bits of its own and redirects system-memory MDLs instead.
    pub(crate) fn bar_segment(&self) -> Option<LocalSegment> {
        self.local_segment().copied()
    }

    /// Which resource occupies the host window at `offset`, if any. The unmap
    /// DDI carries no allocation handle, so offset is the only key it has.
    pub(crate) fn canonical_mapped_resource_at_offset(
        &self,
        offset: u64,
    ) -> Result<Option<u32>, crate::virtio::VirtioError> {
        self.control_owner().mapped_resource_at_offset(offset)
    }

    /// Is this exact resource already mapped at this exact window offset?
    ///
    /// Asked resource-first rather than offset-first because the caller always
    /// holds the allocation, and the offset-keyed direction would need a new
    /// reverse index in the owner table for the same answer. It is the only
    /// question the CPU-host-aperture map path may ask when it arrives above
    /// PASSIVE, where no host round-trip is legal.
    pub(crate) fn resource_mapped_at_offset(&self, resource_id: u32, offset: u64) -> bool {
        matches!(
            self.control_owner().mapped_blob_offset(resource_id),
            Ok(Some(current)) if current == offset
        )
    }

    pub(crate) fn local_segment(&self) -> Option<&LocalSegment> {
        self.transport_generation()
            .and_then(|t| t.local_segment.as_ref())
    }

    pub(crate) fn control_owner(&self) -> &TransportOwner {
        if !crate::virtio::KMD_D2_OWNER_ENABLED {
            unreachable!("the canonical control owner requires the WDDM 3.2 D2 package");
        }
        &self.transport_owner
    }

    pub(crate) fn reset_virtio_physical(
        &self,
        passive: crate::irql::PassiveLevel,
        expected_instance: u64,
    ) -> Result<u32, crate::virtio::VirtioError> {
        if !crate::virtio::KMD_D2_OWNER_ENABLED {
            return Err(crate::virtio::VirtioError::DeviceError);
        }
        let raw = self.d4_dirql_queue.load(Ordering::SeqCst);
        let Some(queue) = NonNull::new(raw as *mut crate::virtio::gpu::InterruptQueue) else {
            return Err(crate::virtio::VirtioError::DeviceError);
        };
        self.d4_dirql_readers.fetch_add(1, Ordering::SeqCst);
        if self.d4_dirql_queue.load(Ordering::SeqCst) != raw {
            self.d4_dirql_readers.fetch_sub(1, Ordering::SeqCst);
            return Err(crate::virtio::VirtioError::DeviceError);
        }
        // SAFETY: the same increment-recheck hazard used by the DIRQL publisher
        // pins the separately boxed queue. Unlike that publisher, this phase
        // retains the caller's real PASSIVE_LEVEL and may therefore perform PCI
        // configuration status access without holding `virtio_lock`.
        let reset = unsafe {
            queue
                .as_ref()
                .reset_status_and_poll(passive, expected_instance)
        };
        self.d4_dirql_readers.fetch_sub(1, Ordering::SeqCst);
        let (status, spins) = reset?;
        // The device is now reset. Re-enter the ordinary transport lock only to
        // retire value/DMA-buffer bookkeeping; no PCI callback or wait occurs
        // in this phase.
        let finished = self
            .with_virtio(|gpu| {
                gpu.finish_physical_reset_and_abort(expected_instance, status, spins)
            })
            .map_err(|_| crate::virtio::VirtioError::DeviceError)??;
        if finished == 0 {
            crate::ddi::native_render::drain_host_terminals(self);
        }
        Ok(finished)
    }

    /// Seal new canonical owner admissions while leaving the live transport
    /// available for orderly DETACH/UNREF/CTX_DESTROY cleanup.
    pub(crate) fn close_control_owner_transport(&self) -> Result<(), crate::virtio::VirtioError> {
        if !crate::virtio::KMD_D2_OWNER_ENABLED {
            return Ok(());
        }
        let expected_instance = match self.with_virtio(|gpu| gpu.scanout_transport_instance()) {
            Ok(instance) => instance,
            Err(_) => return Ok(()),
        };
        self.transport_owner
            .close_for_transport_reset(expected_instance)
    }

    /// Close the disabled owner generation, drain every external runner and
    /// finalizer, verify a real device-status reset, then retire its rows. An
    /// empty transport slot is already quiescent; the dormant owner seed stays
    /// untouched. All waits and MMIO execute outside the owner spinlock.
    pub(crate) fn retire_control_owner_transport(
        &self,
        passive: crate::irql::PassiveLevel,
    ) -> Result<(), crate::virtio::VirtioError> {
        if !crate::virtio::KMD_D2_OWNER_ENABLED {
            return Ok(());
        }
        let expected_instance = match self.with_virtio(|gpu| gpu.scanout_transport_instance()) {
            Ok(instance) => instance,
            Err(_) => return Ok(()),
        };
        self.transport_owner
            .close_for_transport_reset(expected_instance)?;
        let prepared_instance = self.transport_owner.prepare_physical_reset(passive)?;
        if prepared_instance != expected_instance {
            return Err(crate::virtio::VirtioError::DeviceError);
        }
        let raw_status = self.reset_virtio_physical(passive, expected_instance)?;
        self.transport_owner
            .finish_physical_reset(passive, raw_status)
    }

    /// Lock-free observation for query/diagnostic paths. Mutation is exposed
    /// only through [`WddmNotifyGuard`], so advancing the scheduler watermark
    /// statically requires ownership of the notification-lock proof.
    pub(crate) fn completed_fence(&self) -> u32 {
        self.last_completed_fence.load(Ordering::Acquire)
    }

    /// Install one fully initialized transport by consuming the exact proof
    /// minted when this adapter's previous transport was removed.
    ///
    /// # Safety
    /// `absent` must have been minted by the most recent removal on this exact
    /// adapter, and serialized StartDevice ownership must prove the slot stayed
    /// empty. Violating either condition could replace/drop a live device while
    /// the spinlock is held or bind foreign dormant-owner provenance.
    pub(crate) unsafe fn install_virtio(
        &self,
        passive: crate::irql::PassiveLevel,
        absent: TransportAbsent,
        new: Box<VirtioGpu>,
    ) -> Result<(), crate::virtio::VirtioError> {
        debug_assert_eq!(absent.adapter, self as *const Self as usize);
        if !absent.installable
            || self.d4_dirql_transport_retained.load(Ordering::Acquire) != 0
            || self.d4_dirql_readers.load(Ordering::SeqCst) != 0
        {
            return match VirtioGpu::reset_unpublished_or_retain(passive, new) {
                Ok(()) => Err(crate::virtio::VirtioError::DeviceError),
                Err(error) => Err(error),
            };
        }
        let d4_queue = crate::virtio::KMD_D2_OWNER_ENABLED
            .then(|| new.interrupt_queue() as *const crate::virtio::gpu::InterruptQueue as usize)
            .unwrap_or(0);
        // SAFETY: this method's contract binds `new` to this exact adapter and
        // serialized install transition. The observer either remains dormant or
        // constructs/reopens the canonical table from this fully configured
        // local candidate before publication.
        let observation = unsafe {
            self.transport_owner
                .observe_initialized_transport(passive, NonNull::from(self), &new)
        };
        if crate::virtio::KMD_D2_OWNER_ENABLED {
            if let Err(error) = observation {
                // Start/Restart published only this local candidate's ISR byte
                // before entering the install transition. Make the interrupt
                // path inert before a verified reset is allowed to drop it.
                self.isr_status.store(0, Ordering::Release);
                return match VirtioGpu::reset_unpublished_or_retain(passive, new) {
                    Ok(()) => Err(error),
                    Err(reset_error) => Err(reset_error),
                };
            }
        }
        // SAFETY: `virtio_lock` is a valid KSPIN_LOCK; the critical section only
        // installs the already-owned Box (no allocation, no device I/O).
        let irql = unsafe { KeAcquireSpinLockRaiseToDpc(self.virtio_lock.get()) };
        let slot = unsafe { &mut *self.virtio.get() };
        if !slot.is_none()
            || self.d4_dirql_transport_retained.load(Ordering::Acquire) != 0
            || self.d4_dirql_readers.load(Ordering::SeqCst) != 0
        {
            unsafe { KeReleaseSpinLock(self.virtio_lock.get(), irql) };
            self.isr_status.store(0, Ordering::Release);
            return match VirtioGpu::reset_unpublished_or_retain(passive, new) {
                Ok(()) => Err(crate::virtio::VirtioError::DeviceError),
                Err(error) => Err(error),
            };
        }
        *slot = Some(new);
        // Publish while the owning Box is already installed and before the
        // transport lock opens a teardown window. Publishing after unlock
        // would let StopDevice remove/drop the Box and then leave this raw
        // pointer dangling.
        self.d4_dirql_queue.store(d4_queue, Ordering::SeqCst);
        unsafe { KeReleaseSpinLock(self.virtio_lock.get(), irql) };
        Ok(())
    }

    /// Reserve one nonzero direct-plane SET sequence for this transport
    /// generation. The queue stores the sequence in the same fixed slot that
    /// owns the exact candidate and fenced response identity.
    pub(crate) fn reserve_scanout_bind_seq(&self) -> Option<u64> {
        let high_water = self.scanout_bind_next_seq.load(Ordering::Relaxed);
        let next = helios_kmd_logic::scanout_retire::next_bind_sequence(high_water)?;
        self.scanout_bind_next_seq.store(next, Ordering::Relaxed);
        Some(next)
    }

    /// Publish the newest descriptor accepted by the control queue. Both
    /// values are generation-scoped facts only; candidate custody and
    /// completion authority remain in the fixed queue slot.
    pub(crate) fn commit_scanout_bind_seq(&self, sequence: u64, resource_id: u32) {
        self.scanout_bind_wire_resource
            .store(resource_id, Ordering::Relaxed);
        self.scanout_bind_wire_seq
            .store(sequence, Ordering::Release);
    }

    /// Publish one exact D4 SET from the classic DDI's above-DISPATCH arm.
    ///
    /// The raw pointer never escapes this narrow operation. Its lifetime is
    /// paired with `install_virtio` publication and a sequence-consistent hazard
    /// around the withdrawal in
    /// `remove_virtio_and_reset_scanout_bind_generation`.
    pub(crate) unsafe fn enqueue_d4_scanout_dirql(
        &self,
        proof: &crate::ddi::display::SetVidPnDirql<'_>,
        work: crate::ddi::direct_scanout::QueuedDirectScanoutBinding,
    ) -> Result<(), crate::virtio::VirtioError> {
        if !crate::virtio::KMD_D2_OWNER_ENABLED {
            return Err(crate::virtio::VirtioError::DeviceError);
        }
        let raw = self.d4_dirql_queue.load(Ordering::SeqCst);
        let Some(queue) = NonNull::new(raw as *mut crate::virtio::gpu::InterruptQueue) else {
            return Err(crate::virtio::VirtioError::DeviceError);
        };
        self.d4_dirql_readers.fetch_add(1, Ordering::SeqCst);
        if self.d4_dirql_queue.load(Ordering::SeqCst) != raw {
            self.d4_dirql_readers.fetch_sub(1, Ordering::SeqCst);
            return Err(crate::virtio::VirtioError::DeviceError);
        }
        // SAFETY: the hazard increment plus pointer recheck prevents teardown
        // from dropping this Box until the matching decrement below. Queue-core
        // exclusion is enforced independently inside `InterruptQueue`.
        let result = unsafe { queue.as_ref().enqueue_direct_at_dirql(proof, self, work) };
        self.d4_dirql_readers.fetch_sub(1, Ordering::SeqCst);
        result
    }

    pub(crate) fn enqueue_d4_scanout_dispatch(
        &self,
        work: crate::ddi::direct_scanout::QueuedDirectScanoutBinding,
    ) -> Result<(), crate::virtio::VirtioError> {
        if !crate::virtio::KMD_D2_OWNER_ENABLED {
            return Err(crate::virtio::VirtioError::DeviceError);
        }
        self.with_virtio(move |gpu| gpu.enqueue_direct_scanout_dispatch(self, work))
            .map_err(|_| crate::virtio::VirtioError::DeviceError)?
    }

    pub(crate) fn d4_queue_holds_allocation(
        &self,
        handle: usize,
        generation: u64,
        resource_id: u32,
    ) -> bool {
        if self.d4_dirql_transport_retained.load(Ordering::Acquire) != 0 {
            return true;
        }
        match self
            .with_virtio(|gpu| gpu.direct_queue_holds_allocation(handle, generation, resource_id))
        {
            Ok(Ok(holds)) => holds,
            // A live transport whose queue cannot be observed is ambiguous;
            // retain the backing. No transport means no fixed descriptor can
            // still reference it.
            Ok(Err(_)) => true,
            Err(_) => self.d4_dirql_transport_retained.load(Ordering::Acquire) != 0,
        }
    }

    pub(crate) fn d4_dirql_transport_retained(&self) -> bool {
        self.d4_dirql_transport_retained.load(Ordering::Acquire) != 0
    }

    /// Remove the old transport and begin one fresh scanout-bind namespace.
    ///
    /// The four sequence/resource fields are reset while `virtio_lock` proves
    /// the transport absent. A producer that already held the lock drains first;
    /// a later producer observes `None`. Keeping this as one transition prevents
    /// a late old-generation SET from aliasing sequence 1 in the successor.
    #[must_use]
    pub(crate) fn remove_virtio_and_reset_scanout_bind_generation(
        &self,
        passive: crate::irql::PassiveLevel,
    ) -> TransportAbsent {
        self.d4_dirql_queue.store(0, Ordering::SeqCst);
        // SAFETY: `virtio_lock` excludes every producer and the DPC's composite
        // bind apply. Replacing with None before the tuple reset makes any later
        // DPC inert; a DPC already applying drains before this acquire returns.
        let irql = unsafe { KeAcquireSpinLockRaiseToDpc(self.virtio_lock.get()) };
        // Repeat withdrawal under the owner lock. The first store blocks new
        // readers promptly; this one also defeats any concurrent install that
        // published while teardown was waiting to acquire the lock.
        self.d4_dirql_queue.store(0, Ordering::SeqCst);
        // The reader decision belongs under this lock. Installation publishes
        // its pointer before releasing the same lock, so withdrawal cannot
        // observe zero readers and then race a newly published old-generation
        // pointer into existence.
        let d4_quiescent = self.d4_dirql_transport_retained.load(Ordering::Acquire) == 0
            && (!crate::virtio::KMD_D2_OWNER_ENABLED
                || self.d4_dirql_readers.load(Ordering::SeqCst) == 0);
        let old = core::mem::replace(unsafe { &mut *self.virtio.get() }, None);
        if d4_quiescent {
            self.scanout_bind_next_seq.store(0, Ordering::Relaxed);
            self.scanout_bind_wire_seq.store(0, Ordering::Relaxed);
            self.scanout_bind_wire_resource.store(0, Ordering::Relaxed);
        }
        if !d4_quiescent && old.is_some() {
            // Publish the permanent-retain decision before releasing the outer
            // transport lock. A concurrent DestroyAllocation that then observes
            // `virtio=None` must still retain its canonical backing row.
            self.d4_dirql_transport_retained.store(1, Ordering::Release);
        }
        unsafe { KeReleaseSpinLock(self.virtio_lock.get(), irql) };
        let old_owner_instance = old.as_ref().map(|gpu| gpu.scanout_transport_instance());
        if !d4_quiescent && old.is_some() {
            // A reader that crossed withdrawal still owns a reference to the
            // queue. Retain the complete transport and fail all later install /
            // adapter-free attempts rather than race its pointer or custody.
            crate::diag::record_named_bytes(b"D4QRetain", 1);
            core::mem::forget(old);
        } else {
            // Dropped here, at PASSIVE_LEVEL, outside the lock.
            drop(old);
        }
        if d4_quiescent {
            if let Some(physical_instance) = old_owner_instance {
                self.transport_owner.observe_removed_transport(
                    passive,
                    NonNull::from(self),
                    physical_instance,
                );
            } else {
                self.transport_owner
                    .observe_transport_slot_absent(passive, NonNull::from(self));
            }
        }
        TransportAbsent {
            adapter: self as *const Self as usize,
            installable: d4_quiescent,
        }
    }

    /// Install or clear the persistent KMD Venus client, under the PASSIVE
    /// venus mutex (so an in-flight `with_venus_client` cannot be raced by
    /// StopDevice teardown). Device-lifecycle callers only; the previous client
    /// (if any) drops OUTSIDE the mutex, at PASSIVE_LEVEL.
    pub fn set_venus_client(&self, client: Option<crate::virtio::venus::VenusClient>) {
        self.acquire_venus_mutex();
        // SAFETY: the venus mutex gives exclusive access to the cell.
        let old = core::mem::replace(unsafe { &mut *self.venus_client.get() }, client);
        self.release_venus_mutex();
        drop(old);
    }
}

impl Drop for AdapterContext {
    fn drop(&mut self) {
        crate::ddi::diag_etw::adapter_stop(self);
        // Cancel + drain the VSync heartbeat and join the HPD worker before this
        // context's memory (which embeds the KTIMER/KDPC/KEVENT the worker touches)
        // is freed, in case StopDevice was skipped. No-ops if never started.
        // PASSIVE_LEVEL (RemoveDevice).
        self.stop_vsync();
        self.delete_vsync_ex_timer();
        self.stop_hpd();
    }
}
