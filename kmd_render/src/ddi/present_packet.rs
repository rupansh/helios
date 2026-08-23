//! Structural validation and patch-list emission for `DXGKARG_PRESENT`.
//!
//! The allocation list has fixed source/destination slots, but either slot may
//! carry a null handle.  Only a non-null handle is a legal DMA-buffer
//! reference.  Keep that invariant in the type system so neither legacy
//! Present nor PresentToHwQueue can accidentally make a missing allocation
//! resident.

use core::ffi::c_void;
use core::ptr::NonNull;

use helios_protocol::{
    helios_hwa2_swizzle_is_scanout_bindable, D3DDDIFMT_A8R8G8B8, D3DDDI_ID_UNINITIALIZED,
    DXGI_FORMAT_B8G8R8A8_UNORM,
    HELIOS_HWA2_FLAG_CROSS_ADAPTER, HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY,
    HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE, HELIOS_HWA2_FLAG_DISPLAYABLE,
    HELIOS_HWA2_FLAG_PRIMARY, HELIOS_HWA2_FLAG_PROTECTED, HELIOS_HWA2_FLAG_STANDARD,
    HELIOS_HWA2_FLAG_STEREO, HELIOS_HWA2_KIND_IMAGE, HELIOS_HWA2_KIND_STANDARD_PRIMARY,
    HELIOS_HWA2_SWIZZLE_LINEAR, HELIOS_PACKAGE_GENERATION,
};

use crate::dxgk::*;

// Legal retry status for DxgkDdiPresent when either the DMA or patch buffer
// cannot hold the complete operation (ntstatus.h).
pub(crate) const STATUS_GRAPHICS_INSUFFICIENT_DMA_BUFFER: NTSTATUS = 0xC01E_0001u32 as NTSTATUS;

/// Opaque bytes that make an ordinary Present DMA packet non-empty. Submission
/// ordering and allocation identity live in dxgkrnl-owned private data and
/// allocation lists; the DMA payload carries no stream, cookie, or resource id.
pub(crate) const PRESENT_DMA_PACKET_BYTES: usize = core::mem::size_of::<u32>();

/// Byte offset of [`PresentFlipPrivate`] inside the per-context DMA
/// private-data buffer. The established offset remains 32 so the ordinary
/// Present ABI does not move while retired private carriers are deleted.
pub(crate) const PRESENT_FLIP_PRIVATE_OFFSET: usize = 32;

/// `DmaBufferPrivateDataSize` this driver requests per context (`device.rs`'s
/// CreateContext reads it from here). The flip record is exactly 32 bytes and
/// carries only the OS allocation/address/segment/operation pairing.
pub(crate) const PRESENT_DMA_PRIVATE_DATA_BYTES: u32 = 64;

const PRESENT_FLIP_MAGIC: u32 = 0x4850_464C; // "HPFL"
const PRESENT_FLIP_VERSION: u32 = 2;

/// KMD-private flip record for the DMA-BUFFER FLIP contract.
///
/// WHY THIS EXISTS. `DXGK_FLIPCAPS.FlipOnVSyncMmIo` covers only nonzero flip
/// intervals. An IMMEDIATE flip (interval 0 — every unthrottled fullscreen app)
/// goes down the DMA-buffer flip contract instead, in which dxgkrnl calls
/// `DxgkDdiPresent` with a real `pDmaBuffer` and NEVER calls
/// `DxgkDdiSetVidPnSourceAddress`: the driver is expected to program the
/// display itself when that DMA buffer executes.
///
/// Advertising `FlipImmediateMmIo` to force those flips onto the MMIO path
/// instead is NOT a valid substitute for implementing this, and was measured
/// not to be (2026-07-29): the MMIO contract requires the flip to be complete
/// when the DDI RETURNS, and Helios cannot do that, because a Helios flip is a
/// virtio `SET_SCANOUT_BLOB` round-trip that is illegal at the DIRQL the DDI
/// arrives at. Returning STATUS_SUCCESS from a DIRQL stash made dxgkrnl free
/// the previous buffer to the app and issue the next flip while nothing had
/// been programmed — measured as 80 dropped binds and 145 of 1245 present
/// markers writing the buffer that was on screen. The DMA-buffer contract is
/// the one designed for hardware that cannot flip synchronously, because the
/// submission fence puts the completion signal back under driver control.
///
/// It lives in the kernel-only `pDmaBufferPrivateData`, never in the DMA buffer
/// the UMD can see, so the allocation handle it carries is not user-influenced.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct PresentFlipPrivate {
    magic: u32,
    version: u32,
    /// `hDeviceSpecificAllocation` of the flip source. Its immutable per-open
    /// association resolves the same exact allocation identity supplied by
    /// `SetVidPnSourceAddress` on the MMIO path.
    allocation: u64,
    /// `DXGK_ALLOCATIONLIST::PhysicalAddress` of that allocation. This is what
    /// a later CRTC_VSYNC must carry for dxgkrnl to retire the flip.
    physical_address: u64,
    /// Exact allocation-list segment paired with `physical_address`.
    primary_segment: u32,
    /// Exact operation flags for this handoff (zero for the current DMA flip
    /// packet; classic SetVidPn carries its WDK flags directly).
    operation_flags: u32,
}

impl PresentFlipPrivate {
    /// Write the flip record. `private_size` must cover the whole layout; a
    /// smaller buffer is a refusal, never a partial write.
    ///
    /// # Safety
    /// `private_data` points to `private_size` writable bytes supplied by
    /// dxgkrnl for this Present call.
    pub(crate) unsafe fn write(
        private_data: *mut c_void,
        private_size: u32,
        allocation: HANDLE,
        physical_address: u64,
        primary_segment: u32,
        operation_flags: u32,
    ) -> Result<(), NTSTATUS> {
        if private_data.is_null()
            || (private_size as usize)
                < PRESENT_FLIP_PRIVATE_OFFSET + core::mem::size_of::<PresentFlipPrivate>()
        {
            return Err(STATUS_GRAPHICS_INSUFFICIENT_DMA_BUFFER);
        }
        let record = PresentFlipPrivate {
            magic: PRESENT_FLIP_MAGIC,
            version: PRESENT_FLIP_VERSION,
            allocation: allocation as usize as u64,
            physical_address,
            primary_segment,
            operation_flags,
        };
        // SAFETY: the size check above proves the record fits at its offset;
        // unaligned because dxgkrnl makes no alignment promise about the
        // private-data buffer beyond its size.
        unsafe {
            core::ptr::write_unaligned(
                private_data
                    .cast::<u8>()
                    .add(PRESENT_FLIP_PRIVATE_OFFSET)
                    .cast::<PresentFlipPrivate>(),
                record,
            );
        }
        Ok(())
    }

    /// Take a flip record at submit time, or `None` when this DMA buffer
    /// carries none. Validating BOTH magic and version means an uninitialised
    /// or BLT-only private buffer cannot be mistaken for a flip, and the read
    /// CONSUMES the record so a recycled buffer cannot replay it.
    ///
    /// # Safety
    /// `private_data` points to `private_size` writable bytes from the DMA
    /// submission dxgkrnl is handing back.
    pub(crate) unsafe fn take(
        private_data: *mut c_void,
        private_size: u32,
    ) -> Option<(HANDLE, u64, u32, u32)> {
        if private_data.is_null()
            || (private_size as usize)
                < PRESENT_FLIP_PRIVATE_OFFSET + core::mem::size_of::<PresentFlipPrivate>()
        {
            return None;
        }
        let slot = unsafe {
            private_data
                .cast::<u8>()
                .add(PRESENT_FLIP_PRIVATE_OFFSET)
                .cast::<PresentFlipPrivate>()
        };
        // SAFETY: size-checked above; unaligned because dxgkrnl makes no
        // alignment promise about the private-data buffer beyond its size.
        let record = unsafe { core::ptr::read_unaligned(slot) };
        if record.magic != PRESENT_FLIP_MAGIC || record.version != PRESENT_FLIP_VERSION {
            return None;
        }
        if record.allocation == 0 {
            return None;
        }
        // CONSUME IT. dxgkrnl recycles DMA private-data buffers between
        // submissions, so a record left behind would be read again by the next
        // submission that happens to reuse this buffer and would re-arm a bind
        // for a stale — possibly destroyed — allocation. Zeroing the magic
        // makes the record strictly one-shot, which is what "this submission
        // carries a flip" has to mean.
        //
        // SAFETY: same slot, same size check; only the magic word is written.
        unsafe { core::ptr::write_unaligned(slot.cast::<u32>(), 0) };
        Some((
            record.allocation as usize as HANDLE,
            record.physical_address,
            record.primary_segment,
            record.operation_flags,
        ))
    }
}

/// Compile-time proof the two private records fit the per-context private-data
/// buffer (`PRESENT_DMA_PRIVATE_DATA_BYTES`, which `device.rs`'s CreateContext
/// reports). Growing either record past it has to be a deliberate change to
/// BOTH sites, not a silent truncation here.
const _: () = {
    assert!(
        PRESENT_FLIP_PRIVATE_OFFSET + core::mem::size_of::<PresentFlipPrivate>()
            <= PRESENT_DMA_PRIVATE_DATA_BYTES as usize,
        "PresentFlipPrivate does not fit the DMA private-data buffer"
    );
};

#[derive(Clone, Copy)]
pub(crate) struct PresentAllocation {
    handle: NonNull<c_void>,
    allocation_index: u32,
    slot_id: u32,
    driver_id: u32,
    /// `DXGK_ALLOCATIONLIST::PhysicalAddress` — where VidMm has this
    /// allocation right now.
    ///
    /// Carried because the DMA-BUFFER FLIP path has no other source for it.
    /// On the MMIO-flip path `DxgkDdiSetVidPnSourceAddress` hands the driver
    /// the primary address explicitly; in the DMA-flip contract that DDI is
    /// never called, so the allocation list is the only place dxgkrnl states
    /// the address that a later CRTC_VSYNC must match to retire the flip.
    physical_address: u64,
    /// `DXGK_ALLOCATIONLIST::SegmentId`, alongside the address for the same
    /// reason.
    segment_id: u32,
}

impl PresentAllocation {
    #[inline]
    pub(crate) fn handle(self) -> HANDLE {
        self.handle.as_ptr()
    }

    #[inline]
    pub(crate) fn physical_address(self) -> u64 {
        self.physical_address
    }

    #[inline]
    pub(crate) fn segment_id(self) -> u32 {
        self.segment_id
    }
}

/// `DXGK_ALLOCATIONLIST` patch slot ids, and the driver ids we echo back.
///
/// They were bare `1` and `2` written into a bitfield union's raw `Value` with
/// nothing saying what the numbers meant. The values are the WDK's fixed present
/// slots — `DXGK_PRESENT_DESTINATION_INDEX` = 1 and `DXGK_PRESENT_SOURCE_INDEX`
/// = 2 — and the driver id echoes the slot so a patch entry identifies itself.
const PATCH_SLOT_DESTINATION: u32 = DXGK_PRESENT_DESTINATION_INDEX;
const PATCH_SLOT_SOURCE: u32 = DXGK_PRESENT_SOURCE_INDEX;

/// Which arm of `DXGKARG_PRESENT.__bindgen_anon_1` this present carries.
///
/// The union has three arms — `pAllocationList`, `pAllocationInfo` and
/// `pPresentMultiPlaneOverlayInfo` (grep `_DXGKARG_PRESENT__bindgen_ty_1`) — and
/// both call sites used to pick `pAllocationList` implicitly, while
/// `from_allocation_list`'s SAFETY paragraph asserted the array shape without
/// ever having seen the present flags. Decoding the arm ONCE, from
/// `args.Flags`, means the choice happens in one audited place instead of
/// implicitly at two call sites.
pub(crate) enum PresentPayload<'a> {
    /// The fixed source/destination allocation array. Every present this driver
    /// services today.
    AllocationList(PresentAllocationList<'a>),
    /// `FlipWithMultiPlaneOverlay` — the `pPresentMultiPlaneOverlayInfo` arm,
    /// retained with the exact flag/reserved provenance that selected it.
    MultiPlaneOverlay(PresentMpoPayload),
}

impl<'a> PresentPayload<'a> {
    /// `DXGK_PRESENTFLAGS.FlipWithMultiPlaneOverlay`, verified against the
    /// generated bitfield order.
    const FLAG_FLIP_WITH_MPO: u32 = 1 << 12;

    /// Decode the union arm from the present flags.
    ///
    /// # Safety
    /// `args` is dxgkrnl's present argument struct, and the arm named by its
    /// flags is the one it initialised.
    pub(crate) unsafe fn decode(args: &'a DXGKARG_PRESENT) -> Self {
        // SAFETY: `Flags` is a POD union of a bitfield struct and a `UINT`
        // `Value`; reading the `Value` view is a read of initialized memory.
        let flags = unsafe { args.Flags.__bindgen_anon_1.Value };
        if flags & Self::FLAG_FLIP_WITH_MPO != 0 {
            // SAFETY: the MPO flag selected this union arm. Merely carrying the
            // pointer does not dereference it; the disabled owner boundary in
            // `display.rs` dominates every validation read below.
            let info = unsafe { args.__bindgen_anon_1.pPresentMultiPlaneOverlayInfo };
            return Self::MultiPlaneOverlay(PresentMpoPayload {
                info,
                present_flags: flags,
                _present: core::marker::PhantomData,
            });
        }
        // SAFETY: not an MPO present, so the allocation-list arm is live.
        let list = unsafe { args.__bindgen_anon_1.pAllocationList };
        Self::AllocationList(PresentAllocationList {
            list,
            _present: core::marker::PhantomData,
        })
    }
}

/// Closed, counted refusal vocabulary for the bounded D5 Present arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MpoPresentRefusal {
    DisabledOwnerBoundary,
    MpoInfoPointer,
    PlaneListCount,
    PlaneListPointer,
    SourceOrLayer,
    DisabledOrMalformedPlane,
    ReservedBits,
    OpenAllocation,
    AllocationIdentityOrProfile,
    OutputCapacity,
    PacketConstruction,
}

/// The selected MPO union arm before validation. It owns no allocation or
/// display lifetime; it only carries the exact OS pointer and selector fields.
pub(crate) struct PresentMpoPayload {
    info: *mut DXGK_PRESENTMULTIPLANEOVERLAYINFO,
    present_flags: u32,
    _present: core::marker::PhantomData<*const DXGKARG_PRESENT>,
}

#[derive(Clone, Copy)]
struct PresentMpoAllocation {
    open_handle: NonNull<c_void>,
    segment_id: u32,
    physical_address: u64,
}

/// Non-cloneable proof that the complete MPO input and every output capacity
/// were accepted before any output pointer or byte was changed.
pub(crate) struct MpoPresentPacketPlan {
    dma: *mut c_void,
    next_dma: *mut c_void,
    plane: PresentMpoAllocation,
}

const MPO_MAX_PLANES: u32 = 1;
const MPO_PRIVATE_BYTES: usize = 0;
const MPO_PATCH_REFERENCES: usize = 0;
const MPO_PRESENT_RESERVED_FLAGS: u32 = 0xFFFF_C000;
const SHARED_PRIMARY_STANDARD_ALLOCATION_TYPE: u32 = 1;

#[inline]
fn output_capacity<T>(pointer: *mut T, available: usize, required: usize) -> bool {
    required == 0 || (!pointer.is_null() && available >= required)
}

fn exact_mpo_primary_profile(
    facts: &crate::ddi::create_allocation::DirectScanoutAllocationFacts,
    identity: crate::ddi::create_allocation::OpenIdentity,
    source_id: u32,
) -> bool {
    let allocation = &facts.final_hwa2;
    if allocation
        .validate_create_output(HELIOS_PACKAGE_GENERATION)
        .is_err()
        || identity.generation != facts.allocation_generation
        || identity.generation != allocation.allocation_generation
        || identity.kind != allocation.allocation_kind
        || identity.byte_size != allocation.byte_size
        || facts.backing_size < allocation.byte_size
    {
        return false;
    }

    let exact_kind = match allocation.allocation_kind {
        HELIOS_HWA2_KIND_IMAGE => {
            allocation.flags & HELIOS_HWA2_FLAG_STANDARD == 0
                && allocation.standard_allocation_type == 0
        }
        HELIOS_HWA2_KIND_STANDARD_PRIMARY => {
            allocation.flags & HELIOS_HWA2_FLAG_STANDARD != 0
                && allocation.standard_allocation_type == SHARED_PRIMARY_STANDARD_ALLOCATION_TYPE
        }
        _ => false,
    };
    // `DIRECT_FLIP_COMPATIBLE` is required only of the LINEAR arm, which is the
    // one that carries the Direct-Flip WIRE claim. The UMD's direct-scanout
    // primary is `OPAQUE_OPTIMAL`, a class §10.3 rules out for that claim, so
    // `admit_hwa2` never stamps the bit on it and demanding it here would refuse
    // exactly the allocation this path exists to flip.
    let required_flags = HELIOS_HWA2_FLAG_PRIMARY
        | HELIOS_HWA2_FLAG_DISPLAYABLE
        | if allocation.swizzle_class == HELIOS_HWA2_SWIZZLE_LINEAR {
            HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE
        } else {
            0
        };
    let forbidden_flags =
        HELIOS_HWA2_FLAG_STEREO | HELIOS_HWA2_FLAG_PROTECTED | HELIOS_HWA2_FLAG_CROSS_ADAPTER;
    let exact_source = if allocation.flags & HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY != 0 {
        allocation.vidpn_source == D3DDDI_ID_UNINITIALIZED
    } else {
        allocation.vidpn_source == source_id
    };

    exact_kind
        && allocation.flags & required_flags == required_flags
        && allocation.flags & forbidden_flags == 0
        && allocation.dxgi_format == DXGI_FORMAT_B8G8R8A8_UNORM
        && allocation.d3d_ddi_format == D3DDDIFMT_A8R8G8B8
        && allocation.depth_or_array_size == 1
        && allocation.mip_levels == 1
        && allocation.sample_count == 1
        && allocation.sample_quality == 0
        && allocation.plane_count == MPO_MAX_PLANES
        && helios_hwa2_swizzle_is_scanout_bindable(allocation.swizzle_class)
        && exact_source
}

impl PresentMpoPayload {
    /// Validate the exact one-primary WDK 28000 payload and all output capacity.
    /// The owner boundary must be checked before calling this routine.
    ///
    /// # Safety
    /// The payload came from the active MPO arm of a live `DXGKARG_PRESENT`.
    pub(crate) unsafe fn prepare_mpo_present(
        self,
        args: &DXGKARG_PRESENT,
    ) -> Result<MpoPresentPacketPlan, MpoPresentRefusal> {
        if self.info.is_null() || !self.info.is_aligned() {
            return Err(MpoPresentRefusal::MpoInfoPointer);
        }
        // SAFETY: null/alignment were checked before forming this reference;
        // dxgkrnl owns the input for the duration of the DDI.
        let info = unsafe { &*self.info };
        if info.PlaneListCount != MPO_MAX_PLANES {
            return Err(MpoPresentRefusal::PlaneListCount);
        }
        if info.VidPnSourceId != 0 {
            return Err(MpoPresentRefusal::SourceOrLayer);
        }
        if self.present_flags & MPO_PRESENT_RESERVED_FLAGS != 0 {
            return Err(MpoPresentRefusal::ReservedBits);
        }
        if self.present_flags != PresentPayload::FLAG_FLIP_WITH_MPO {
            return Err(MpoPresentRefusal::DisabledOrMalformedPlane);
        }
        let plane_pointer = info.pPlaneList;
        if plane_pointer.is_null() || !plane_pointer.is_aligned() {
            return Err(MpoPresentRefusal::PlaneListPointer);
        }
        // `PlaneListCount == 1` was established before this reference. No slice
        // or pointer arithmetic is needed for the frozen one-plane profile.
        let plane = unsafe { &*plane_pointer };
        if plane.LayerIndex != 0 {
            return Err(MpoPresentRefusal::SourceOrLayer);
        }
        if plane.Enabled != 1 {
            return Err(MpoPresentRefusal::DisabledOrMalformedPlane);
        }
        if plane.__bindgen_anon_1.Reserved() != 0 {
            return Err(MpoPresentRefusal::ReservedBits);
        }

        let open_handle = plane.hDeviceSpecificAllocation;
        let Some((_allocation_handle, facts)) = (unsafe {
            crate::ddi::create_allocation::open_direct_scanout_allocation_facts(open_handle)
        }) else {
            return Err(MpoPresentRefusal::OpenAllocation);
        };
        let Some(identity) =
            (unsafe { crate::ddi::create_allocation::open_allocation_identity(open_handle) })
        else {
            return Err(MpoPresentRefusal::OpenAllocation);
        };
        if !crate::adapter::allocation_object::is_current(identity.generation) {
            return Err(MpoPresentRefusal::OpenAllocation);
        }
        if !exact_mpo_primary_profile(&facts, identity, info.VidPnSourceId) {
            return Err(MpoPresentRefusal::AllocationIdentityOrProfile);
        }
        let Some(open_handle) = NonNull::new(open_handle) else {
            return Err(MpoPresentRefusal::OpenAllocation);
        };
        let plane = PresentMpoAllocation {
            open_handle,
            segment_id: plane.__bindgen_anon_1.SegmentId(),
            // SAFETY: PhysicalAddress is the selected address view in this WDK
            // record; keep it paired with the handle and segment above.
            physical_address: unsafe { plane.PhysicalAddress.QuadPart as u64 },
        };

        let dma_bytes = PRESENT_DMA_PACKET_BYTES;
        if !output_capacity(args.pDmaBuffer, args.DmaSize as usize, dma_bytes)
            || !output_capacity(
                args.pDmaBufferPrivateData,
                args.DmaBufferPrivateDataSize as usize,
                MPO_PRIVATE_BYTES,
            )
            || !output_capacity(
                args.pPatchLocationListOut,
                args.PatchLocationListOutSize as usize,
                MPO_PATCH_REFERENCES,
            )
        {
            return Err(MpoPresentRefusal::OutputCapacity);
        }
        let Some(next_dma_address) = (args.pDmaBuffer as usize).checked_add(dma_bytes) else {
            return Err(MpoPresentRefusal::PacketConstruction);
        };
        Ok(MpoPresentPacketPlan {
            dma: args.pDmaBuffer,
            next_dma: next_dma_address as *mut c_void,
            plane,
        })
    }
}

impl MpoPresentPacketPlan {
    /// Emit the already planned ordinary Present command. No failure remains
    /// after this point, so a refusal cannot leave a partial packet or cursor.
    ///
    /// # Safety
    /// `prepare_mpo_present` proved the DMA pointer and size for this same
    /// `args` object.
    pub(crate) unsafe fn emit_mpo_present(self, args: &mut DXGKARG_PRESENT) {
        // Consume the exact handle/segment/address tuple as one value. The WDK
        // MPO list itself is dxgkrnl's allocation and placement contract, and
        // inventing an allocation-list index in the DMA bytes would be a
        // different packet ABI.
        let _exact_os_plane = (
            self.plane.open_handle.as_ptr(),
            self.plane.segment_id,
            self.plane.physical_address,
        );
        unsafe {
            core::ptr::write_unaligned(self.dma.cast::<u32>(), 0);
        }
        args.pDmaBuffer = self.next_dma;
        args.MultipassOffset = 0;
    }
}

/// The fixed present allocation array, as a value only [`PresentPayload::decode`]
/// can produce.
///
/// A wrapper rather than a raw pointer so the OTHER two union arms cannot be
/// reinterpreted as an allocation list. It adds provenance, not a new runtime
/// check: the count it uses is the WDK's fixed `DXGK_PRESENT_MAX_INDEX + 1`, NOT
/// `NumSrcAllocations`/`NumDstAllocations`, and it must stay that way — dxgkrnl
/// supplies the full fixed array and encodes an absent source or destination as
/// a NULL `hDeviceSpecificAllocation`. Passing the Num* counts as the bound
/// "while we're here" changes which slots are read for the absent case and
/// silently drops presents.
pub(crate) struct PresentAllocationList<'a> {
    list: *mut DXGK_ALLOCATIONLIST,
    _present: core::marker::PhantomData<&'a DXGKARG_PRESENT>,
}

impl PresentAllocationList<'_> {
    /// Whether dxgkrnl supplied an array at all (`PBalst`/`PHQalst`).
    pub(crate) fn is_present(&self) -> bool {
        !self.list.is_null()
    }
}

#[derive(Clone, Copy)]
pub(crate) struct PresentAllocations {
    source: Option<PresentAllocation>,
    destination: Option<PresentAllocation>,
}

impl PresentAllocations {
    /// Decode the fixed DXGK present allocation slots.
    ///
    /// A source or destination that is not part of this operation is encoded
    /// by dxgkrnl as a null `hDeviceSpecificAllocation`.  Converting only
    /// non-null handles to `PresentAllocation` makes it impossible for patch
    /// emission to reference an absent allocation.
    ///
    /// # Safety
    /// `list` came from [`PresentPayload::decode`], so it is the fixed present
    /// allocation array supplied by dxgkrnl and contains the source and
    /// destination indices defined by the WDK.
    pub(crate) unsafe fn from_allocation_list(list: &PresentAllocationList<'_>) -> Self {
        let allocation_list = list.list;
        if allocation_list.is_null() {
            return Self {
                source: None,
                destination: None,
            };
        }

        // SAFETY: both reads index the fixed WDK present slots of the array
        // `PresentPayload::decode` validated, and the address/segment fields
        // are plain data in the same entry as the handle.
        let source_entry = unsafe { &*allocation_list.add(DXGK_PRESENT_SOURCE_INDEX as usize) };
        let destination_entry =
            unsafe { &*allocation_list.add(DXGK_PRESENT_DESTINATION_INDEX as usize) };
        let source_handle = source_entry.hDeviceSpecificAllocation;
        let destination_handle = destination_entry.hDeviceSpecificAllocation;
        // `PhysicalAddress` and `VirtualAddress` are a union; this driver's
        // present allocations are physically addressed (the GpuMmu model is
        // decorative — see `gpummu.rs`), so the physical arm is the correct one.
        let address_of = |entry: &DXGK_ALLOCATIONLIST| -> u64 {
            // SAFETY: reading the PhysicalAddress arm of the address union.
            unsafe { entry.__bindgen_anon_2.PhysicalAddress.as_ref().QuadPart as u64 }
        };

        Self {
            source: NonNull::new(source_handle).map(|handle| PresentAllocation {
                handle,
                allocation_index: DXGK_PRESENT_SOURCE_INDEX,
                slot_id: PATCH_SLOT_SOURCE,
                driver_id: PATCH_SLOT_SOURCE,
                physical_address: address_of(source_entry),
                segment_id: source_entry.__bindgen_anon_1.SegmentId(),
            }),
            destination: NonNull::new(destination_handle).map(|handle| PresentAllocation {
                handle,
                allocation_index: DXGK_PRESENT_DESTINATION_INDEX,
                slot_id: PATCH_SLOT_DESTINATION,
                driver_id: PATCH_SLOT_DESTINATION,
                physical_address: address_of(destination_entry),
                segment_id: destination_entry.__bindgen_anon_1.SegmentId(),
            }),
        }
    }

    #[inline]
    pub(crate) fn source(self) -> Option<PresentAllocation> {
        self.source
    }

    #[inline]
    pub(crate) fn destination(self) -> Option<PresentAllocation> {
        self.destination
    }

    #[inline]
    pub(crate) fn reference_count(self) -> usize {
        usize::from(self.destination.is_some()) + usize::from(self.source.is_some())
    }

    /// Validate patch-list capacity without mutating the runtime's output
    /// cursor. Present must call this before it submits any host GPU work, so an
    /// insufficient-buffer retry cannot duplicate a BLT.
    ///
    /// Returns the proof, not `()`. [`PatchCapacity`] is non-`Copy` and
    /// non-`Clone` and carries the checked pointer and count, so
    /// [`Self::write_patch_references`] cannot run without it, cannot re-derive
    /// the capacity expression, and cannot be called twice against a stale
    /// cursor. Same shape as `WddmNotifyGuard` and T1b's `RenderGdiPlan`.
    pub(crate) fn validate_patch_capacity(
        self,
        args: &DXGKARG_PRESENT,
    ) -> Result<PatchCapacity, NTSTATUS> {
        let required = self.reference_count();
        if required == 0 {
            return Ok(PatchCapacity {
                first: core::ptr::null_mut(),
                required: 0,
            });
        }
        if args.pPatchLocationListOut.is_null()
            || (args.PatchLocationListOutSize as usize) < required
        {
            return Err(STATUS_GRAPHICS_INSUFFICIENT_DMA_BUFFER);
        }
        Ok(PatchCapacity {
            first: args.pPatchLocationListOut,
            required,
        })
    }

    /// Emit exactly the allocation references represented by this value.
    ///
    /// Consumes the [`PatchCapacity`] token, so the "validate before any host
    /// GPU work" ordering is discharged by the type system rather than by the
    /// programmer remembering: `display.rs` validates at the top and submits the
    /// BLT before writing, and moving the submission above the validation — or
    /// adding a second one after it — no longer compiles into a path that can
    /// duplicate GPU copies on an insufficient-buffer retry (the same defect
    /// class as T1b's `k-paging-01`). `scheduler.rs` acquires the token
    /// immediately before writing, which is what documents that it queues
    /// nothing.
    ///
    /// # Safety
    /// The output patch list is supplied by dxgkrnl; `capacity` records that we
    /// checked the count dxgkrnl declared, not that the memory is mapped.
    pub(crate) unsafe fn write_patch_references(
        self,
        capacity: PatchCapacity,
        args: &mut DXGKARG_PRESENT,
    ) -> Result<(), NTSTATUS> {
        let PatchCapacity { first, required } = capacity;
        if required == 0 {
            return Ok(());
        }

        let mut written = 0usize;
        for reference in [self.destination, self.source].into_iter().flatten() {
            let patch = unsafe { first.add(written) };
            unsafe {
                core::ptr::write_bytes(patch, 0, 1);
                (*patch).AllocationIndex = reference.allocation_index;
                (*patch).__bindgen_anon_1.Value = reference.slot_id;
                (*patch).DriverId = reference.driver_id;
            }
            written += 1;
        }

        // `written == required` by construction: both are derived from the same
        // two Options, and `PatchCapacity` carries the count computed from them.
        // The `debug_assert_eq!` that used to stand here was a LIVE KeBugCheck
        // site in the shipped image — [profile.dev] does not disable
        // debug-assertions and cargo-make ships that profile — so it traded a
        // structurally-impossible mismatch for a real bugcheck.
        args.pPatchLocationListOut = unsafe { first.add(written) };
        // Decrement the declared capacity with the cursor. A second call would
        // otherwise validate against a stale count. `display.rs` records
        // PatchLocationListOutSize into PBPatch BEFORE the write, so the
        // breadcrumb is unaffected — keep it that way.
        args.PatchLocationListOutSize =
            args.PatchLocationListOutSize.saturating_sub(written as u32);
        Ok(())
    }
}

/// Proof that the patch-list capacity was checked, carrying what was checked.
///
/// Non-`Copy` and non-`Clone` on purpose: it is consumed by
/// [`PresentAllocations::write_patch_references`], so it cannot be reused for a
/// second write against an advanced cursor.
///
/// The token must CARRY the pointer and count and the writer must stop
/// re-deriving them — otherwise this is a relocation of the same runtime check.
/// That is why the capacity expression now exists exactly once, in
/// [`PresentAllocations::validate_patch_capacity`], instead of being duplicated
/// there and in the writer where the two copies could silently diverge.
pub(crate) struct PatchCapacity {
    /// The checked `pPatchLocationListOut`. Null iff `required == 0`.
    first: *mut D3DDDI_PATCHLOCATIONLIST,
    /// How many entries were proven to fit.
    required: usize,
}
