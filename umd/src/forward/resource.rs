//! Resource create / open / destroy / resolve, and WDDM allocation.
//!
//! `make_resident`, `allocate_wddm_resource`, `finish_wddm_tex2d`,
//! `create_resource`, `open_resource` and the shared-resource resolvers.
//!
//! Moved verbatim out of `forward.rs` by T8/R1107.

use super::*;

// --- CalcPrivate*Size (all store one COM pointer) ---------------------------

pub(crate) unsafe extern "C" fn calc_size_resource(
    _h: Hdevice,
    _a: *const ddi::D3D11DDIARG_CREATERESOURCE,
) -> u64 {
    8
}
pub(crate) unsafe extern "C" fn calc_size_rtv(
    _h: Hdevice,
    _a: *const ddi::D3D10DDIARG_CREATERENDERTARGETVIEW,
) -> u64 {
    8
}

// --- Resources --------------------------------------------------------------

pub(crate) const RES_BUFFER: ddi::D3D10DDIRESOURCE_TYPE =
    ddi::D3D10DDIRESOURCE_TYPE_D3D10DDIRESOURCE_BUFFER;
pub(crate) const RES_BUFFEREX: ddi::D3D10DDIRESOURCE_TYPE =
    ddi::D3D10DDIRESOURCE_TYPE_D3D11DDIRESOURCE_BUFFEREX;
pub(crate) const RES_TEX2D: ddi::D3D10DDIRESOURCE_TYPE =
    ddi::D3D10DDIRESOURCE_TYPE_D3D10DDIRESOURCE_TEXTURE2D;
pub(crate) const RES_TEX1D: ddi::D3D10DDIRESOURCE_TYPE =
    ddi::D3D10DDIRESOURCE_TYPE_D3D10DDIRESOURCE_TEXTURE1D;
pub(crate) const RES_TEX3D: ddi::D3D10DDIRESOURCE_TYPE =
    ddi::D3D10DDIRESOURCE_TYPE_D3D10DDIRESOURCE_TEXTURE3D;
pub(crate) const RES_TEXCUBE: ddi::D3D10DDIRESOURCE_TYPE =
    ddi::D3D10DDIRESOURCE_TYPE_D3D10DDIRESOURCE_TEXTURECUBE;

/// The resource dimensions `create_resource` implements, as a closed set.
///
/// `D3D10DDIRESOURCE_TYPE` is a bindgen integer, so the conversion stays
/// fallible and exactly one counted catch-all survives at the conversion. The
/// guarantee is that no KNOWN dimension can be silently dropped by a match that
/// quietly grew a hole, not that the integer domain becomes closed.
#[derive(Copy, Clone, PartialEq, Eq)]
pub(crate) enum ResourceDimension {
    Buffer,
    Texture1D,
    Texture2D,
    Texture3D,
}

impl ResourceDimension {
    pub(crate) fn from_ddi(dimension: ddi::D3D10DDIRESOURCE_TYPE) -> Option<Self> {
        match dimension {
            RES_BUFFER | RES_BUFFEREX => Some(Self::Buffer),
            RES_TEX1D => Some(Self::Texture1D),
            RES_TEX2D | RES_TEXCUBE => Some(Self::Texture2D),
            RES_TEX3D => Some(Self::Texture3D),
            _ => None,
        }
    }
}

/// Add one allocation to the WDDM 2.x device residency list.
///
/// E_PENDING is completed with a blocking monitored-fence wait through the
/// runtime callback. No command referencing the allocation may be submitted
/// before that fence reaches `PagingFenceValue`.
pub(crate) unsafe fn make_resident(
    dev: &crate::device_funcs::HeliosDevice,
    handle: ddi::D3DKMT_HANDLE,
) -> Result<ResidentAllocation, i32> {
    const E_PENDING: i32 = 0x8000_000Au32 as i32;

    let Some(handle) = core::num::NonZeroU32::new(handle) else {
        log_error!("WDDM residency: zero allocation handle");
        return Err(E_INVALIDARG);
    };
    let Some(queue) = dev.paging_queue else {
        log_error!("WDDM residency: device has no paging queue");
        return Err(E_FAIL);
    };
    if dev.kt_callbacks.is_null() {
        log_error!("WDDM residency: no runtime callbacks");
        return Err(E_FAIL);
    }
    let Some(make_resident_cb) = (*dev.kt_callbacks).pfnMakeResidentCb else {
        log_error!("WDDM residency: pfnMakeResidentCb missing");
        return Err(E_FAIL);
    };
    let Some(evict_cb) = (*dev.kt_callbacks).pfnEvictCb else {
        // Do not acquire a residency reference that cannot be balanced.
        log_error!("WDDM residency: pfnEvictCb missing");
        return Err(E_FAIL);
    };

    let allocation = handle.get();
    let mut arg = ddi::D3DDDI_MAKERESIDENT::default();
    arg.hPagingQueue = queue.handle.get();
    arg.NumAllocations = 1;
    arg.AllocationList = &allocation;
    let hr = make_resident_cb(dev.h_rt_device, &mut arg);
    trace_line!(
        "WDDM residency: MakeResident alloc=0x{:x} hr=0x{:08x} fence={} trim={}",
        allocation,
        hr as u32,
        arg.PagingFenceValue,
        arg.NumBytesToTrim
    );
    if hr != 0 && hr != E_PENDING {
        log_error!(
            "WDDM residency: MakeResident FAILED alloc=0x{:x} hr=0x{:08x} trim={}",
            allocation,
            hr as u32,
            arg.NumBytesToTrim
        );
        return Err(hr);
    }

    let resident = ResidentAllocation {
        handle,
        h_rt_device: dev.h_rt_device,
        evict_cb,
    };
    if hr == E_PENDING {
        let Some(wait_cb) = (*dev.kt_callbacks).pfnWaitForSynchronizationObjectFromCpuCb else {
            log_error!("WDDM residency: E_PENDING but CPU fence-wait callback is missing");
            drop(resident);
            return Err(E_FAIL);
        };
        let sync_object = queue.sync_object.get();
        let fence_value = arg.PagingFenceValue;
        let mut wait = ddi::D3DDDICB_WAITFORSYNCHRONIZATIONOBJECTFROMCPU::default();
        wait.ObjectCount = 1;
        wait.ObjectHandleArray = &sync_object;
        wait.FenceValueArray = &fence_value;
        // hAsyncEvent == NULL selects the blocking, non-polling form.
        let wait_hr = wait_cb(dev.h_rt_device, &wait);
        trace_line!(
            "WDDM residency: wait alloc=0x{:x} fence={} observed={} hr=0x{:08x}",
            allocation,
            fence_value,
            queue.fence_value_cpu.as_ptr().read_volatile(),
            wait_hr as u32
        );
        if wait_hr != 0 {
            log_error!(
                "WDDM residency: paging-fence wait FAILED alloc=0x{:x} fence={} hr=0x{:08x}",
                allocation,
                fence_value,
                wait_hr as u32
            );
            drop(resident);
            return Err(wait_hr);
        }
    }

    Ok(resident)
}

pub(crate) unsafe fn deallocate_standalone(
    dev: &crate::device_funcs::HeliosDevice,
    allocation: ddi::D3DKMT_HANDLE,
) {
    if dev.kt_callbacks.is_null() {
        return;
    }
    let Some(deallocate_cb) = (*dev.kt_callbacks).pfnDeallocateCb else {
        return;
    };
    let mut allocation = allocation;
    let mut arg = ddi::D3DDDICB_DEALLOCATE {
        hResource: core::ptr::null_mut(),
        NumAllocations: 1,
        HandleList: &mut allocation,
    };
    let hr = deallocate_cb(dev.h_rt_device, &mut arg);
    log_error!(
        "DDI allocate rollback: alloc=0x{:x} hr=0x{:08x}",
        allocation,
        hr as u32
    );
}

pub(crate) unsafe fn allocate_wddm_resource(
    h: Hdevice,
    a: &ddi::D3D11DDIARG_CREATERESOURCE,
    mip0: &ddi::D3D10DDI_MIPINFO,
    h_rt: ddi::D3D10DDI_HRTRESOURCE,
    // `Some` = venus-backed (the KMD adopts our allocation), `None` = plain
    // KMD-backed standard allocation. This one `Option` replaces the three
    // independent `backing_blob_id != 0` conjunctions.
    backing: Option<VenusBacking>,
    // Kept separate from `scanout` on purpose -- see alloc.rs.
    direct_scanout_primary: bool,
    // Scan-out primary metadata. LINEAR paths use the queried COLOR row pitch;
    // direct OPTIMAL uses a logical scanout stride while QEMU validates the
    // opaque allocation with its exact Vulkan allocation size.
    scanout: Option<ScanoutGeometry>,
) -> Result<(Option<ResidentAllocation>, ddi::D3DKMT_HANDLE), i32> {
    const DDI_BIND_PRESENT: u32 = 0x0000_0080;

    let needs_allocation = needs_wddm_texture_allocation(a);
    if !needs_allocation {
        return Ok((None, 0));
    }

    let Some(dev) = helios_device(h) else {
        return Err(E_FAIL);
    };
    if dev.kt_callbacks.is_null() {
        log_error!("DDI allocate_wddm_resource: no KT callbacks");
        return Err(E_FAIL);
    }
    let Some(allocate_cb) = (*dev.kt_callbacks).pfnAllocateCb else {
        log_error!("DDI allocate_wddm_resource: pfnAllocateCb missing");
        return Err(E_FAIL);
    };

    // ⛔⛔ K4 / `K4-CONTRACT.md` §5 — the resid gap, at the producer end.
    //
    // A `Some(backing)` create is the one this driver used to answer by putting
    // the venus resource id in `HeliosWddmAllocPrivate::adopt_resource_id` and
    // asking the kernel to ADOPT the memory DXVK had already allocated. HWA2
    // carries no host resource id, no venus context id and no blob id, and
    // §10.3 forbids adding one: "no UMD, ICD, batch, or private descriptor can
    // name or supply" a host resid — it may live SOLELY inside the KMD
    // allocation object.
    //
    // There is no substitution available and inventing one is explicitly out of
    // bounds. Proceeding without the id is worse than refusing: the kernel
    // would allocate its OWN blob, `stamp_dxvk_resource_kmt_handles` would then
    // bind a KMT handle to a DXVK image whose memory that blob does not
    // describe, and the result is the two-memory split — a resident, storable,
    // real-handled alias that renders black forever (15th session). That is
    // fake success; this is loud failure.
    //
    // ⇒ Refuse, count, and name the successor mechanism: **Mesa lane unit A3**
    // (plus K6) — the ICD stops naming host resources at all and the KMD
    // patches the resid in from `HeliosNativeRenderPatch`. Until A3 lands,
    // every shared / keyed-mutex / present / primary D3D11 texture create
    // fails here. §5 records that as the retirement's intended intermediate
    // state, not a regression.
    if let Some(b) = backing {
        note_ddi_refusal(&DDI_REFUSALS.hwa2_create_venus_backing_needs_mesa_a3);
        log_error!(
            "DDI allocate_wddm_resource REFUSED: venus-backed create needs an adopted host \
             resource id, which HWA2 cannot carry (blob=0x{:x} res_id={} blob_size={} \
             {}x{} fmt={} bind=0x{:x} misc=0x{:x} primary={} direct_scanout={}) -> \
             blocked on mesa unit A3 (+K6): the ICD stops naming host resources and the KMD \
             patches the resid in from HeliosNativeRenderPatch",
            b.blob_id.get(),
            b.resource_id.get(),
            b.blob_size,
            mip0.TexelWidth,
            mip0.TexelHeight,
            a.Format,
            a.BindFlags,
            a.MiscFlags,
            !a.pPrimaryDesc.is_null(),
            direct_scanout_primary,
        );
        return Err(E_OUTOFMEMORY);
    }

    const CROSS_ADAPTER_PITCH_ALIGN: u32 = 256;
    let raw_pitch = mip0
        .TexelWidth
        .saturating_mul(dxgi_bytes_per_pixel(a.Format as u32));
    let pitch =
        raw_pitch.saturating_add(CROSS_ADAPTER_PITCH_ALIGN - 1) & !(CROSS_ADAPTER_PITCH_ALIGN - 1);
    // A LINEAR scan-out primary reports its exact COLOR row pitch; use it verbatim so
    // `SET_SCANOUT_BLOB` reads rows at the true host stride instead of the
    // cross-adapter guess (a wrong stride shears the scanned-out image).
    let pitch = match scanout {
        Some(g) => g.pitch.get(),
        None => pitch,
    };
    // ⚠ `backing` is `None` from here down — the `Some` arm returned above — so
    // the "a live blob_size overrides the computed linear size" fallback that
    // used to sit here has no reachable input. The extent HWA2 records is the
    // one this UMD can compute, and it is byte-for-byte the value the retired
    // `HeliosWddmAllocPrivate::size` carried.
    //
    // ⭐ DECIDED — `K4-CONTRACT.md` §1.3, and this arm is **Tier 2**. The KMD
    // computes a KMD-side linear extent of its own — `create_allocation.rs`
    // `linear_blob_size(pitch, height)` = `pitch * round_up(height, 128) + 64
    // KiB`, floor one page — from two constants that are private to
    // `kmd_render` and deliberately NOT in `protocol/`. That estimate is
    // *strictly larger* than the number computed here, which is exactly why the
    // question needed a ruling rather than a guess.
    //
    // §1.3's answer for a descriptor this driver authored (Tier 2, i.e. NOT a
    // `STANDARD` allocation the KMD wrote itself): the KMD requires `byte_size
    // != 0`, every plane record bounded inside it, and `byte_size <=` the
    // backing extent it created. **Exceeding the backing is refused; the reverse
    // is admitted, and nothing is rewritten — the UMD's claim stands or the
    // create fails.** It does NOT demand equality with `linear_blob_size` on
    // this arm; had it done so, every D3D11 create would have failed. So the
    // extent this driver can actually compute is the right thing to send.
    let size = (pitch as u64)
        .saturating_mul(mip0.TexelHeight.max(1) as u64)
        .max(4096);

    // pPrimaryDesc is the runtime's authoritative primary classification.
    // A dedicated-copy source is intentionally OPTIMAL and has no scanout
    // pitch, but it still must be a WDDM primary or DXGI rejects every Flip
    // before DxgkDdiPresent with "Source of Flip must be primary".
    //
    // ⛔ It is also the ONLY source for HWA2's `vidpn_source` (offset 80): a
    // conventional D3D11 primary must carry its concrete source and everything
    // else must carry `D3DDDI_ID_UNINITIALIZED`, cross-validated by `validate`.
    let primary_vidpn_source = (!a.pPrimaryDesc.is_null()).then(|| (*a.pPrimaryDesc).VidPnSourceId);

    // The four dimensions HWA2's kind enum can express. `create_resource`
    // already refused an unknown one (`unhandled_resource_dimension`), so this
    // is unreachable through the DDI — counted anyway, because the alternative
    // to a counted refusal here is allocating with a GUESSED allocation kind.
    let Some(dimension) = ResourceDimension::from_ddi(a.ResourceDimension) else {
        note_ddi_refusal(&DDI_REFUSALS.hwa2_unknown_dimension);
        log_error!(
            "DDI allocate_wddm_resource REFUSED: dimension {} has no HWA2 allocation kind",
            a.ResourceDimension
        );
        return Err(E_INVALIDARG);
    };

    let input = Hwa2CreateInput {
        dimension,
        // `ResourceDimension` collapses RES_TEXCUBE into Texture2D, so the cube
        // signal has to be read from the raw DDI value before that conversion.
        texture_cube: a.ResourceDimension == RES_TEXCUBE,
        texel_width: mip0.TexelWidth,
        texel_height: mip0.TexelHeight,
        texel_depth: mip0.TexelDepth,
        array_size: a.ArraySize,
        mip_levels: a.MipLevels,
        // The creator's EXACT DXGI format, so a cross-process opener rebuilds
        // the image with the same bpp/layout instead of a squashed BGRA. The
        // lossy D3DDDIFORMAT travels beside it for `DxgkDdiDescribeAllocation`.
        dxgi_format: a.Format as u32,
        d3d_ddi_format: dxgi_to_d3dddi_format(a.Format as u32),
        sample_count: a.SampleDesc.Count,
        sample_quality: a.SampleDesc.Quality,
        ddi_bind_flags: a.BindFlags,
        ddi_misc_flags: a.MiscFlags,
        byte_size: size,
        row_pitch: pitch,
        // Scan-out primary's real memory-plane-0 offset (0 for everything
        // else); the KMD adds it to the blob base in SET_SCANOUT_BLOB.
        plane_offset: scanout.map_or(0, |g| g.plane_offset),
        primary_vidpn_source,
        direct_scanout_primary,
    };

    let mut desc = match input.build() {
        Ok(desc) => desc,
        Err(refusal) => {
            // One counter per reason — the caller of a refused create needs to
            // know WHICH field could not be expressed, not that "something" was.
            match refusal {
                Hwa2InputRefusal::ImageDxgiFormatUnknown => {
                    note_ddi_refusal(&DDI_REFUSALS.hwa2_image_format_unknown)
                }
                Hwa2InputRefusal::ByteSizeZero | Hwa2InputRefusal::PlaneUnrepresentable { .. } => {
                    note_ddi_refusal(&DDI_REFUSALS.hwa2_plane_unrepresentable)
                }
            }
            log_error!(
                "DDI allocate_wddm_resource REFUSED: cannot build HWA2 create input: {:?} \
                 ({}x{} fmt={} pitch={} size={} bind=0x{:x} misc=0x{:x})",
                refusal,
                mip0.TexelWidth,
                mip0.TexelHeight,
                a.Format,
                pitch,
                size,
                a.BindFlags,
                a.MiscFlags
            );
            return Err(E_INVALIDARG);
        }
    };

    // The producer's own gate. `validate_create_input` is the input half of the
    // two-stage HWA2 contract (`K4-CONTRACT.md` §1.1): it enforces the shared
    // cross-field core PLUS `allocation_generation == 0` and the two KMD-owned
    // flag bits clear. Running it here is not belt-and-braces — the KMD refuses
    // the create rather than correcting a field, so a descriptor that fails it
    // is a create that was going to fail anyway, and failing it HERE names the
    // field instead of surfacing an opaque callback HRESULT.
    if let Err(rejection) = desc.validate_create_input(HELIOS_PACKAGE_GENERATION) {
        note_ddi_refusal(&DDI_REFUSALS.hwa2_input_invalid);
        log_error!(
            "DDI allocate_wddm_resource REFUSED: own HWA2 create input is invalid: {:?} \
             (kind={} flags=0x{:x} bind=0x{:x} misc=0x{:x} {}x{} dxgi={} planes={} size={})",
            rejection,
            desc.allocation_kind,
            desc.flags,
            desc.bind_flags,
            desc.misc_flags,
            desc.width,
            desc.height,
            desc.dxgi_format,
            desc.plane_count,
            desc.byte_size
        );
        return Err(E_INVALIDARG);
    }

    let pre_n = WDDM_ALLOC_LOG_COUNT.peek();
    if pre_n < 128 {
        log_error!(
            "DDI allocate_wddm_resource pre: HWA2 kind={} flags=0x{:x} bind=0x{:x} misc=0x{:x} \
             size={} {}x{}x{} mips={} dxgi={} d3dddi={} samples={}x{} vidpn={} swizzle={} \
             memory={} planes={} pitch={} plane_off={} pkg_gen=0x{:x}",
            desc.allocation_kind,
            desc.flags,
            desc.bind_flags,
            desc.misc_flags,
            desc.byte_size,
            desc.width,
            desc.height,
            desc.depth_or_array_size,
            desc.mip_levels,
            desc.dxgi_format,
            desc.d3d_ddi_format,
            desc.sample_count,
            desc.sample_quality,
            desc.vidpn_source,
            desc.swizzle_class,
            desc.memory_class,
            desc.plane_count,
            desc.planes[0].row_pitch,
            desc.planes[0].offset,
            desc.package_generation
        );
    }

    // The descriptor exactly as it goes in, for the write-back diff below.
    // `HeliosWddmAllocationDescV2` is `Copy`, so this is a value snapshot.
    //
    // ⚠ Taken BEFORE `private_ptr` is derived, deliberately: reading the local
    // `desc` place after taking `&mut desc` would invalidate the raw pointer the
    // kernel is about to write through, and every later read of `desc` is
    // AFTER the callback has returned.
    let sent = desc;

    let mut allocation_info = ddi::D3DDDI_ALLOCATIONINFO2::default();
    let private_ptr = (&mut desc as *mut HeliosWddmAllocationDescV2).cast();
    // ⛔ Exactly `HELIOS_HWA2_BYTES`. `from_private_data` requires the length to
    // be exact on the far side (`==`, not `>=`), so 96 -> 168 is a hard ABI
    // step, not a growth an older KMD tolerates.
    let private_size = u32::from(HELIOS_HWA2_BYTES);
    allocation_info.pPrivateDriverData = private_ptr;
    allocation_info.PrivateDriverDataSize = private_size;
    let is_present = (a.BindFlags & DDI_BIND_PRESENT) != 0;
    let is_primary_allocation = !a.pPrimaryDesc.is_null();
    allocation_info.VidPnSourceId = if !a.pPrimaryDesc.is_null() {
        (*a.pPrimaryDesc).VidPnSourceId
    } else {
        0
    };
    // A pPrimaryDesc resource is a real WDDM primary regardless of whether its
    // backing is directly scannable or copied into the KMD-owned LINEAR target.
    allocation_info.Flags.Value = if is_primary_allocation { 1 } else { 0 };
    let mut alloc = ddi::D3DDDICB_ALLOCATE::default();
    // pfnAllocateCb expects the runtime resource handle for the resource whose
    // surfaces are being allocated. Shared resources additionally return an
    // hKMResource, but the association itself is not optional for present-only
    // allocations.
    alloc.pPrivateDriverData = private_ptr;
    alloc.PrivateDriverDataSize = private_size;
    alloc.hResource = h_rt.handle;
    alloc.NumAllocations = 1;
    alloc.__bindgen_anon_1.pAllocationInfo2 = &mut allocation_info;

    let hr = allocate_cb(dev.h_rt_device, &mut alloc);
    let h_allocation = allocation_info.hAllocation;
    let n = WDDM_ALLOC_LOG_COUNT.next();
    if n < 128 || hr != 0 {
        log_error!(
            "DDI allocate_wddm_resource: hr=0x{:08x} alloc=0x{:x} km=0x{:x} rt={:p} assoc={:p} \
             info={} rpriv={} size={} pitch={} kind={} primary={} present={} vidpn={} {}x{} \
             fmt={} bind=0x{:x} misc=0x{:x} alloc_gen=0x{:x}",
            hr as u32,
            h_allocation,
            alloc.hKMResource,
            h_rt.handle,
            alloc.hResource,
            allocation_info.PrivateDriverDataSize,
            alloc.PrivateDriverDataSize,
            size,
            pitch,
            desc.allocation_kind,
            is_primary_allocation,
            is_present,
            allocation_info.VidPnSourceId,
            mip0.TexelWidth,
            mip0.TexelHeight,
            a.Format,
            a.BindFlags,
            a.MiscFlags,
            desc.allocation_generation
        );
    }
    if hr != 0 {
        return Err(hr);
    }

    // ── the write-back, checked (K4 obligation 2) ───────────────────────────
    //
    // ⛔ Before K4 NOTHING checked what came back. The old code diffed six
    // fields into a log line and then used the allocation regardless, so a KMD
    // that wrote nothing, wrote a stale generation, or silently corrected a
    // field was indistinguishable from one that agreed with us.
    //
    // `validate_create_output` is the output half of the two-stage contract: the
    // same cross-field core as `validate_create_input`, plus the requirement
    // that `allocation_generation` is now NONZERO — i.e. the kernel really did
    // stamp this buffer. A descriptor that fails it describes a resource we do
    // not have, so the allocation is rolled back and the create fails; keeping
    // it would leave `store_resource` holding a handle whose descriptor nobody
    // can trust, which is exactly the disagreement §10.3 exists to prevent.
    //
    // ⚠ UNEXERCISED AND KNOWN TO BE. `FINDINGS.md` F10 answered the general
    // question — dxgkrnl DOES copy the KMD's create-time private write back into
    // the creating UMD's buffer, proven by a pre/post byte comparison in which a
    // 48-byte kernel-authored record arrives intact — but read F10's **Bound**:
    // that proof covers the D3D11 arm at a **96-byte** `PrivateDriverDataSize`
    // and explicitly NOT 168. This arm sends exactly `HELIOS_HWA2_BYTES` = 168
    // (`private_size`, above), so the length that was measured is not the length
    // that ships, and propagation AT THIS LENGTH is still unproven. If it does
    // not hold, every create fails here with `AllocationGenerationZero` — the
    // loud, correct symptom of a design assumption being false, and why this
    // refusal names the field.
    //
    // The instruments that answer it on the deployed build are this file's own
    // `hwa2_output_invalid` (below — deliberately broad: zero generation,
    // mutated echo, or package-generation mismatch alike) and, on the D3D12
    // arm, `Hwa2WriteBackAbsent` in `umd12/src/forward12/resource12.rs`.
    // ⛔ NOT `AllocPrivateWrittenBack`: F10's "the instrument that could NOT
    // have answered it" section records why — it fired only when the write-back
    // DIFFERED from what the UMD sent, so "no write" and "the write agreed with
    // me" were indistinguishable — and at HEAD it has been re-graded into a
    // success census (expected to equal `IdentityRecorded`), not a detector.
    if let Err(rejection) = desc.validate_create_output(HELIOS_PACKAGE_GENERATION) {
        note_ddi_refusal(&DDI_REFUSALS.hwa2_output_invalid);
        log_error!(
            "DDI allocate_wddm_resource REFUSED: HWA2 write-back is invalid: {:?} \
             (alloc=0x{:x} km=0x{:x} alloc_gen=0x{:x} pkg_gen=0x{:x} kind={} flags=0x{:x} \
             size={} {}x{}) -> deallocating",
            rejection,
            h_allocation,
            alloc.hKMResource,
            desc.allocation_generation,
            desc.package_generation,
            desc.allocation_kind,
            desc.flags,
            desc.byte_size,
            desc.width,
            desc.height
        );
        unsafe { deallocate_standalone(dev, h_allocation) };
        return Err(E_OUTOFMEMORY);
    }

    // The KMD echoes every field it validated and stamps only the two it owns
    // (`allocation_generation`, and the `DIRECT_FLIP_COMPATIBLE` /
    // `D3D12_RUNTIME_PRIMARY` bits). Anything else that moved is a contract
    // violation on the kernel side rather than an error here — it is not
    // refused, because the descriptor is still internally valid and refusing
    // would hide which field moved behind a dead resource. It IS logged with
    // both values, because "the KMD corrected a field" is precisely what §1.1
    // says cannot happen.
    //
    // The mask is `protocol`'s, not a local re-derivation of it. This site used
    // to declare `const KMD_OWNED_FLAGS = DIRECT_FLIP_COMPATIBLE |
    // D3D12_RUNTIME_PRIMARY`, and so did three others (`protocol` itself,
    // `kmd_logic`, `umd12`): one rule, four declarations, nothing comparing
    // them. Add a third KMD-owned bit and every hand copy keeps clearing two —
    // the echo check then reports a mismatch on a field the KMD legitimately
    // stamped, which is a false finding in the one place whose whole job is to
    // be believed. Imported here rather than through `forward.rs`'s re-export
    // block because this is the only consumer in the crate.
    if n < 128 {
        use helios_protocol::HELIOS_HWA2_FLAG_KMD_OWNED_MASK;
        let echoed = HeliosWddmAllocationDescV2 {
            allocation_generation: sent.allocation_generation,
            flags: desc.flags & !HELIOS_HWA2_FLAG_KMD_OWNED_MASK,
            ..desc
        };
        if echoed != sent {
            log_error!(
                "DDI allocate_wddm_resource: KMD did NOT echo the create input verbatim \
                 (alloc=0x{:x}) sent kind={} flags=0x{:x} bind=0x{:x} misc=0x{:x} size={} \
                 {}x{} dxgi={} d3dddi={} vidpn={} swizzle={} memory={} planes={} pitch={} \
                 -> got kind={} flags=0x{:x} bind=0x{:x} misc=0x{:x} size={} {}x{} dxgi={} \
                 d3dddi={} vidpn={} swizzle={} memory={} planes={} pitch={}",
                h_allocation,
                sent.allocation_kind,
                sent.flags,
                sent.bind_flags,
                sent.misc_flags,
                sent.byte_size,
                sent.width,
                sent.height,
                sent.dxgi_format,
                sent.d3d_ddi_format,
                sent.vidpn_source,
                sent.swizzle_class,
                sent.memory_class,
                sent.plane_count,
                sent.planes[0].row_pitch,
                desc.allocation_kind,
                desc.flags,
                desc.bind_flags,
                desc.misc_flags,
                desc.byte_size,
                desc.width,
                desc.height,
                desc.dxgi_format,
                desc.d3d_ddi_format,
                desc.vidpn_source,
                desc.swizzle_class,
                desc.memory_class,
                desc.plane_count,
                desc.planes[0].row_pitch,
            );
        }
    }

    match unsafe { make_resident(dev, h_allocation) } {
        Ok(resident) => Ok((Some(resident), alloc.hKMResource)),
        Err(resident_hr) => {
            // pfnAllocateCb succeeded, so this UMD owns the allocation even
            // though residency failed. Roll it back before surfacing the
            // failure; no partially initialized ResourceState is created.
            unsafe { deallocate_standalone(dev, h_allocation) };
            Err(resident_hr)
        }
    }
}

/// Common tail for a freshly created 2D texture resource (normal or scan-out
/// primary): record the venus backing identity, make the paired WDDM/KMD
/// allocation (carrying the scan-out row pitch + plane offset for a primary),
/// transfer venus-resource ownership to that allocation, KMT-stamp the DXVK
/// resource, and store it in the DDI handle. `scanout_pitch`/`scanout_offset`
/// are either the LINEAR COLOR layout or the direct-OPTIMAL logical scanout
/// metadata; they are 0/0 for non-primary resources.
pub(crate) unsafe fn finish_wddm_tex2d(
    h: Hdevice,
    a: &ddi::D3D11DDIARG_CREATERESOURCE,
    mip0: &ddi::D3D10DDI_MIPINFO,
    h_rt: ddi::D3D10DDI_HRTRESOURCE,
    h_resource: ddi::D3D10DDI_HRESOURCE,
    res: ID3D11Resource,
    direct_scanout_primary: bool,
    scanout: Option<ScanoutGeometry>,
) {
    let (memory, memory_size, memory_offset, resource_id) = dxvk_resource_memory_info(h, &res);
    let needs_importable = needs_wddm_texture_allocation(a);
    let (backing_blob_id, backing_blob_size, backing_resource_id) = if memory != 0
        && memory_offset == 0
        && memory <= u32::MAX as u64
    {
        (memory, memory_size, resource_id)
    } else {
        // Only resources that get a WDDM allocation (shared / keyed-mutex /
        // present / primary) NEED an importable backing — for those a
        // suballocated DXVK memory means a cross-process opener sees a
        // disconnected KMD blob (two-memory split), so shout. Private
        // textures are suballocated by design (18th session).
        if memory != 0 && needs_importable {
            log_error!(
                    "DDI create_resource(tex2d): SHARED RESOURCE WITHOUT IMPORTABLE BACKING memory=0x{:x} res_id={} size={} offset={} bind=0x{:x} misc=0x{:x}",
                    memory, resource_id, memory_size, memory_offset, a.BindFlags, a.MiscFlags
                );
        } else if memory != 0 {
            trace_line!(
                "DDI create_resource(tex2d): private suballocated memory=0x{:x} size={} offset={}",
                memory,
                memory_size,
                memory_offset
            );
        }
        (0, 0, 0)
    };
    // The creating `vkAllocateMemory`'s exact size + memory type.
    //
    // ⛔ K4: these NO LONGER reach any allocation private data. HWA2 carries no
    // `venus_alloc_size` and §10.3 is explicit that the Vulkan memory-type index
    // is gone ("no Vulkan memory-type index … the retired trailer carried
    // `memory_type_index`; it is gone"). Both are still read here for exactly
    // one surviving consumer — `SnapshotSourceDesc` below, the windowed-BLT
    // snapshot descriptor, which is a UMD-internal record that never crosses
    // the allocation seam.
    //
    // ⚠ `global_vidmm_tracker` is now a WRITE-ONLY sink. The `GlobalVidMmTracker`
    // system-wide KMT share + cookie has NO successor at all
    // (`K4-CONTRACT.md` §6: `wddm_legacy.rs:30`'s claim that it was "folded into
    // HWA2's own tracking-kind fields" is false — HWA2 has no tracking kind, no
    // cookie, no global-share field and no tracker flag bit, and §10.3's "no …
    // independently usable identity" forbids reintroducing it under another
    // name). The out-parameter survives only because the C++ bridge declares it
    // (`umd/bridge/`, not this lane). Removing it must change the Rust extern
    // and the C++ declaration in ONE changeset or the bridge stops linking —
    // that is an open cross-lane request against `umd/bridge/`, and §6 rules
    // only that the tracker has no successor; it does not itself cover the
    // bridge signature.
    let (mut venus_alloc_size, mut memory_type_index) = (0u64, 0u32);
    let mut retired_global_vidmm_tracker = 0u64;
    if backing_resource_id != 0 {
        if let Some(dev) = helios_device(h) {
            if !dev.dxvk.get_resource_alloc_identity(
                res.as_raw() as usize,
                &mut venus_alloc_size,
                &mut memory_type_index,
                &mut retired_global_vidmm_tracker,
            ) {
                log_error!(
                    "DDI create_resource(tex2d): no venus alloc identity for res_id={}",
                    backing_resource_id
                );
            }
        }
    }
    let backing = VenusBacking::new(backing_blob_id, backing_blob_size, backing_resource_id);
    // A shared/present/primary texture must bind its WDDM allocation to the
    // exact exportable Venus resource that backs the DXVK image. Falling back
    // to a fresh KMD blob would create two disconnected allocations; treating
    // a bare VkDeviceMemory id as a blob id is worse, because virglrenderer
    // rejects the non-exportable memory and destroys the Venus context.
    if needs_importable && backing.is_none() {
        log_error!(
            "DDI create_resource(tex2d): SHARED RESOURCE WITHOUT IMPORTABLE BACKING memory=0x{:x} res_id={} size={} offset={} bind=0x{:x} misc=0x{:x} -> refused",
            memory,
            resource_id,
            memory_size,
            memory_offset,
            a.BindFlags,
            a.MiscFlags
        );
        set_runtime_error(h, E_OUTOFMEMORY);
        return;
    }
    let (allocation, km_resource) =
        match allocate_wddm_resource(h, a, mip0, h_rt, backing, direct_scanout_primary, scanout) {
            Ok(allocation) => allocation,
            Err(hr) => {
                log_error!(
                    "DDI create_resource(tex2d): WDDM allocation/residency failed hr=0x{:08x}",
                    hr as u32
                );
                set_runtime_error(h, hr);
                return;
            }
        };
    let allocation_handle = allocation
        .as_ref()
        .map(ResidentAllocation::handle)
        .unwrap_or(0);
    // ⚠ K4: UNREACHABLE until mesa unit A3 lands, and deliberately kept.
    // `backing_resource_id != 0` implies `backing.is_some()`, and
    // `allocate_wddm_resource` refuses that create outright (HWA2 cannot name a
    // host resource id — see its `hwa2_create_venus_backing_needs_mesa_a3`
    // refusal), so this arm has no reachable input today. It is the ownership
    // hand-off the adopt path needs the moment the KMD starts patching the
    // resid in from `HeliosNativeRenderPatch`; deleting it would silently drop
    // that hand-off from the A3 changeset.
    if allocation_handle != 0 && backing_resource_id != 0 {
        if let Some(dev) = helios_device(h) {
            // SAFETY: `res` is the live resource this DDI just created.
            if !unsafe { dev.dxvk.transfer_resource_ownership(res.as_raw() as usize) } {
                log_error!(
                    "DDI create_resource(tex2d): ownership transfer failed res_id={}",
                    backing_resource_id
                );
            }
        }
    }
    trace_line!(
        "DDI create_resource(tex2d): before KMT stamp km=0x{:x} alloc=0x{:x} blob=0x{:x} res_id={} blob_size={}",
        km_resource,
        allocation_handle,
        backing_blob_id,
        backing_resource_id,
        backing_blob_size
    );
    stamp_dxvk_resource_kmt_handles(h, &res, allocation_handle, km_resource);
    let snapshot_source = core::num::NonZeroU32::new(backing_resource_id).and_then(|resource_id| {
        (venus_alloc_size != 0 && mip0.TexelWidth != 0 && mip0.TexelHeight != 0).then_some(
            SnapshotSourceDesc {
                resource_id: resource_id.get(),
                venus_alloc_size,
                memory_type_index,
                width: mip0.TexelWidth,
                height: mip0.TexelHeight,
                dxgi_format: a.Format as u32,
            },
        )
    });
    // Only the exact runtime-designated primary may identify itself through
    // Present private data. Ordinary resource identity stays in
    // `snapshot_source` above, never in this rotation-coupled field.
    let present_private = match (
        direct_scanout_primary,
        core::num::NonZeroU32::new(backing_resource_id),
        scanout,
    ) {
        (true, Some(resource_id), Some(geometry)) => HeliosPresentPrivateData {
            plane_offset: geometry.plane_offset,
            magic: HELIOS_PRESENT_PRIVATE_MAGIC,
            version: HELIOS_PRESENT_PRIVATE_VERSION,
            resource_id: resource_id.get(),
            width: mip0.TexelWidth,
            height: mip0.TexelHeight,
            pitch: geometry.pitch.get(),
            dxgi_format: a.Format as u32,
            reserved: HELIOS_PRESENT_PRIVATE_FLAG_DIRECT_SCANOUT,
            venus_alloc_size: 0,
            // The direct-primary creation path has no relationship to the
            // registered present signal. The per-present eligible path alone
            // fills this appended stream-correlation tail.
            present_ctx_id: 0,
            present_value: 0,
            present_cookie: 0,
            snapshot_memory_type_index: 0,
            snapshot_purpose: HELIOS_PRESENT_SNAPSHOT_PURPOSE_NONE,
        },
        _ => empty_present_private(),
    };
    if RESOURCE_LOG_COUNT.first_n(128).is_some() {
        trace_line!(
            "DDI create_resource(tex2d) ok after-stamp-call: {}x{} fmt={} usage={} bind=0x{:x} misc=0x{:x} sample={}x{}",
            mip0.TexelWidth, mip0.TexelHeight, a.Format, a.Usage, a.BindFlags,
            a.MiscFlags, a.SampleDesc.Count, a.SampleDesc.Quality
        );
    }
    store_resource(
        h_resource,
        res,
        allocation,
        km_resource,
        h_rt.handle,
        AllocationOwnership::CreatedByUmd, // via pfnAllocateCb above
        present_private,
        snapshot_source,
    );
    if present_private.is_valid() {
        unsafe { remember_direct_scanout_allocation(h, allocation_handle, present_private) };
    }
}

pub(crate) unsafe extern "C" fn create_resource(
    h: Hdevice,
    arg: *const ddi::D3D11DDIARG_CREATERESOURCE,
    h_resource: ddi::D3D10DDI_HRESOURCE,
    h_rt: ddi::D3D10DDI_HRTRESOURCE,
) {
    clear_handle(h_resource);
    if arg.is_null() {
        log_error!("DDI CreateResource identity: null args");
        set_runtime_error(h, E_INVALIDARG);
        return;
    }
    let Some(device) = d3d11_device(h) else {
        return;
    };
    let a = &*arg;
    let mip0 = if a.pMipInfoList.is_null() {
        ddi::D3D10DDI_MIPINFO {
            TexelWidth: 0,
            TexelHeight: 0,
            TexelDepth: 0,
            PhysicalWidth: 0,
            PhysicalHeight: 0,
            PhysicalDepth: 0,
        }
    } else {
        *a.pMipInfoList
    };

    // Build initial-data array if provided (one entry per subresource).
    let num_sub = (a.MipLevels.max(1) * a.ArraySize.max(1)) as usize;
    if let Some(identity_n) =
        CREATE_RESOURCE_IDENTITY_LOG_COUNT.first_n_then_every_from_one(512, 2048)
    {
        trace_line!(
            "DDI CreateResource identity: #{} hDrv={:p} hRT={:p} hDevice={:p} \
             dim={} fmt={} texel={}x{}x{} physical={}x{}x{} usage={} map=0x{:x} \
             bind=0x{:x} misc=0x{:x} mips={} array={} sample={}x{} byte_stride={} \
             decoder_type={} texture_layout={} initial={:p}/{} primary={:p}",
            identity_n,
            h_resource.pDrvPrivate,
            h_rt.handle,
            h.pDrvPrivate,
            a.ResourceDimension,
            a.Format,
            mip0.TexelWidth,
            mip0.TexelHeight,
            mip0.TexelDepth,
            mip0.PhysicalWidth,
            mip0.PhysicalHeight,
            mip0.PhysicalDepth,
            a.Usage,
            a.MapFlags,
            a.BindFlags,
            a.MiscFlags,
            a.MipLevels,
            a.ArraySize,
            a.SampleDesc.Count,
            a.SampleDesc.Quality,
            a.ByteStride,
            a.DecoderBufferType,
            a.TextureLayout,
            a.pInitialDataUP,
            if a.pInitialDataUP.is_null() {
                0
            } else {
                num_sub
            },
            a.pPrimaryDesc,
        );
        if !a.pPrimaryDesc.is_null() {
            let primary = &*a.pPrimaryDesc;
            let mode = &primary.ModeDesc;
            trace_line!(
                "DDI CreateResource primary: #{} hRT={:p} flags=0x{:x} vidpn={} \
                 mode={}x{} fmt={} refresh={}/{} scanline={} rotation={} scaling={} \
                 driver_flags=0x{:x}",
                identity_n,
                h_rt.handle,
                primary.Flags,
                primary.VidPnSourceId,
                mode.Width,
                mode.Height,
                mode.Format,
                mode.RefreshRate.Numerator,
                mode.RefreshRate.Denominator,
                mode.ScanlineOrdering,
                mode.Rotation,
                mode.Scaling,
                primary.DriverFlags,
            );
        }
        if !a.pInitialDataUP.is_null() {
            for subresource in 0..num_sub.min(16) {
                let up = &*a.pInitialDataUP.add(subresource);
                trace_line!(
                    "DDI CreateResource initial: #{} subresource={} sysmem={:p} \
                     row_pitch={} slice_pitch={}",
                    identity_n,
                    subresource,
                    up.pSysMem,
                    up.SysMemPitch,
                    up.SysMemSlicePitch,
                );
            }
        }
    }
    let mut init: Vec<D3D11_SUBRESOURCE_DATA> = Vec::new();
    if !a.pInitialDataUP.is_null() {
        for i in 0..num_sub {
            let up = &*a.pInitialDataUP.add(i);
            init.push(D3D11_SUBRESOURCE_DATA {
                pSysMem: up.pSysMem,
                SysMemPitch: up.SysMemPitch,
                SysMemSlicePitch: up.SysMemSlicePitch,
            });
        }
    }
    let init_ptr = if init.is_empty() {
        None
    } else {
        Some(init.as_ptr())
    };

    let cpu = cpu_access(a.Usage);

    let Some(dimension) = ResourceDimension::from_ddi(a.ResourceDimension) else {
        // The DDI returns void and `clear_handle` already nulled the slot, so
        // logging and returning told the runtime S_OK with a null driver
        // resource. Every later `load_resource` then returns None and the view
        // create / Map / Copy silently does nothing — "nothing draws", not an
        // error. E_INVALIDARG is in CreateTexture*'s documented return set.
        note_ddi_refusal(&DDI_REFUSALS.unhandled_resource_dimension);
        log_error!(
            "DDI create_resource: unhandled dimension {}",
            a.ResourceDimension
        );
        set_runtime_error(h, E_INVALIDARG);
        return;
    };
    match dimension {
        ResourceDimension::Buffer => {
            let bind = api_bind_flags(a.BindFlags);
            let misc = api_misc_flags(a.MiscFlags, a.BindFlags, true);
            if bind != a.BindFlags || misc != a.MiscFlags || !a.pPrimaryDesc.is_null() {
                log_error!(
                    "DDI create_resource(buffer): normalize bind 0x{:x}->0x{:x} misc 0x{:x}->0x{:x} primary={}",
                    a.BindFlags,
                    bind,
                    a.MiscFlags,
                    misc,
                    !a.pPrimaryDesc.is_null()
                );
            }
            let desc = D3D11_BUFFER_DESC {
                ByteWidth: mip0.TexelWidth,
                Usage: D3D11_USAGE(a.Usage as i32),
                BindFlags: bind,
                CPUAccessFlags: cpu,
                MiscFlags: misc,
                StructureByteStride: a.ByteStride,
            };
            let (allocation, km_resource) =
                match allocate_wddm_resource(h, a, &mip0, h_rt, None, false, None) {
                    Ok(allocation) => allocation,
                    Err(hr) => {
                        log_error!(
                        "DDI create_resource(buffer): WDDM allocation/residency failed hr=0x{:08x}",
                        hr as u32
                    );
                        set_runtime_error(h, hr);
                        return;
                    }
                };
            let allocation_handle = allocation
                .as_ref()
                .map(ResidentAllocation::handle)
                .unwrap_or(0);
            let mut buf: Option<ID3D11Buffer> = None;
            let created = device.CreateBuffer(&desc, init_ptr, Some(&mut buf));
            if let Err(ref e) = created {
                log_error!("DDI create_resource(buffer) failed: {e:?}");
            }
            let res = match buf {
                Some(b) => match b.cast::<ID3D11Resource>() {
                    Ok(r) => Some(r),
                    Err(e) => {
                        log_error!(
                            "DDI create_resource(buffer): cast to ID3D11Resource failed: {e:?}"
                        );
                        None
                    }
                },
                None => {
                    if created.is_ok() {
                        log_error!(
                            "DDI create_resource(buffer): DXVK CreateBuffer returned no buffer"
                        );
                    }
                    None
                }
            };
            let mut stored = false;
            finish_create(h, created, res, |res| {
                stored = true;
                stamp_dxvk_resource_kmt_handles(h, &res, allocation_handle, km_resource);
                if RESOURCE_LOG_COUNT.first_n(128).is_some() {
                    log_error!(
                        "DDI create_resource(buffer) ok: bytes={} fmt={} usage={} bind=0x{:x} misc=0x{:x}",
                        mip0.TexelWidth, a.Format, a.Usage, bind, misc
                    );
                }
                store_resource(
                    h_resource,
                    res,
                    allocation,
                    km_resource,
                    h_rt.handle,
                    AllocationOwnership::CreatedByUmd, // via pfnAllocateCb above
                    empty_present_private(),
                    None,
                );
            });
            if !stored && allocation_handle != 0 {
                // The buffer arm allocates first and creates second, so a failed
                // CreateBuffer (or a missing object, or a failed cast) leaves a
                // kernel allocation nobody can reach: the closure's
                // `Option<ResidentAllocation>` has been dropped by now — evicting
                // the residency reference — but pfnDeallocateCb was never called,
                // and `release_resource` cannot recover it later because
                // `clear_handle` already nulled pDrvPrivate at DDI entry, so
                // DestroyResource reads a null state pointer and returns. The
                // handle would stay associated with the runtime resource until
                // device teardown.
                //
                // Evict-then-deallocate is the order `release_resource`
                // documents, and dropping the closure above already did the
                // evict half. Same call `allocate_wddm_resource` makes for its
                // own rollback.
                if let Some(dev) = helios_device(h) {
                    deallocate_standalone(dev, allocation_handle);
                }
            }
        }
        ResourceDimension::Texture2D => {
            let bind = api_bind_flags(a.BindFlags);
            let mut misc = api_misc_flags(a.MiscFlags, a.BindFlags, false);
            if a.ResourceDimension == RES_TEXCUBE {
                misc |= D3D11_RESOURCE_MISC_TEXTURECUBE.0 as u32;
            }
            if a.MiscFlags != 0 {
                log_error!(
                    "DDI misc translation v2: ddi_misc=0x{:x} ddi_bind=0x{:x} api_misc=0x{:x}",
                    a.MiscFlags,
                    a.BindFlags,
                    misc
                );
            }
            if bind != a.BindFlags
                || (misc & !D3D11_RESOURCE_MISC_TEXTURECUBE.0 as u32) != a.MiscFlags
                || !a.pPrimaryDesc.is_null()
            {
                log_error!(
                    "DDI create_resource(tex2d): {}x{} fmt={} usage={} bind 0x{:x}->0x{:x} misc 0x{:x}->0x{:x} primary={} sample={}x{}",
                    mip0.TexelWidth,
                    mip0.TexelHeight,
                    a.Format,
                    a.Usage,
                    a.BindFlags,
                    bind,
                    a.MiscFlags,
                    misc,
                    !a.pPrimaryDesc.is_null(),
                    a.SampleDesc.Count,
                    a.SampleDesc.Quality
                );
            }
            let desc = D3D11_TEXTURE2D_DESC {
                Width: mip0.TexelWidth,
                Height: mip0.TexelHeight,
                MipLevels: a.MipLevels,
                ArraySize: a.ArraySize.max(1),
                Format: DXGI_FORMAT(a.Format as i32),
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: a.SampleDesc.Count,
                    Quality: a.SampleDesc.Quality,
                },
                Usage: D3D11_USAGE(a.Usage as i32),
                BindFlags: bind,
                CPUAccessFlags: cpu,
                MiscFlags: misc,
            };
            // Windows' pPrimaryDesc is the authoritative, non-heuristic marker
            // for a scan-out primary. The supported 32-bit Windows primary
            // formats become dedicated OPTIMAL DMA_BUF exports.
            let is_scanout = !a.pPrimaryDesc.is_null() && matches!(a.Format as u32, 28 | 87 | 88);
            let mut handled = false;
            if is_scanout {
                // The QEMU fork reconstructs this exact same-driver OPTIMAL
                // DMA_BUF with the original blob allocation size. This avoids a
                // guest copy and any virtio protocol field or global modifier
                // extension. The host display backend currently reads it back.
                // The wrapper adopts the reference and returns the metadata
                // with it, so `rp`/`off` cannot be read on the failure path.
                // R813.
                let created = helios_device(h).and_then(|dev| {
                    dev.dxvk.create_scanout_texture2d(
                        mip0.TexelWidth,
                        mip0.TexelHeight,
                        a.Format as u32,
                        bind,
                        misc,
                        false,
                    )
                });
                let (rp, off) = created.as_ref().map_or((0, 0), |(_, p, o)| (*p, *o));
                // BEHAVIOUR CHANGE (R806 sub-commit 2): a zero row pitch is a
                // failed scan-out-primary create, not a primary with no
                // geometry. Previously only `raw != 0` was checked, so a
                // non-zero resource with `rp == 0` would stamp
                // HELIOS_WDDM_ALLOC_MISC_PRIMARY | MISC_DIRECT_SCANOUT into the
                // KMD meta while finish_wddm_tex2d's present_private gate
                // failed -- a direct scan-out primary in the kernel that the
                // UMD never registered in direct_scanout_allocations and could
                // never identify through PresentCb private data. Nothing
                // detected that split state.
                //
                // Not reachable through today's bridge: create_ddi_scanout_
                // texture2d returns 0 for a zero width/height and otherwise
                // computes a non-zero pitch, so raw != 0 implies rp != 0. This
                // closes the cross-FFI contract dependency rather than a live
                // bug, which is why the counter is expected to stay 0.
                let geometry = ScanoutGeometry::new(rp as u32, off);
                // One match over the pair, so the created resource is moved
                // into exactly one arm and the refusal arm still owns it (and
                // therefore still releases it).
                match (created, geometry) {
                    (Some((res, _, _)), Some(geometry)) => {
                        log_error!(
                        "DDI create_resource(tex2d): direct scan-out primary {}x{} fmt={} logicalPitch={} offset={} (OPTIMAL DMA_BUF)",
                        mip0.TexelWidth, mip0.TexelHeight, a.Format, rp, off
                    );
                        finish_wddm_tex2d(h, a, &mip0, h_rt, h_resource, res, true, Some(geometry));
                    }
                    (created, _) => {
                        // Loud failure over fake success: do NOT fall back to a plain
                        // primary — that reintroduces the black scan-out as a "working"
                        // desktop. A failure here is a real direct-scanout regression.
                        if let Some((res, _, _)) = created {
                            // The new arm: the bridge handed back a resource but no
                            // usable stride. Dropping the adopted wrapper releases
                            // it -- nothing else will. R813 removed the manual
                            // IUnknown::from_raw this used to need.
                            SCANOUT_PRIMARY_ZERO_PITCH.fetch_add(1, Ordering::Relaxed);
                            let raw = res.as_raw() as usize;
                            drop(res);
                            log_error!(
                            "DDI create_resource(tex2d): SCAN-OUT PRIMARY ZERO PITCH {}x{} fmt={} raw=0x{:x} offset={} -> refused (zero_pitch={})",
                            mip0.TexelWidth,
                            mip0.TexelHeight,
                            a.Format,
                            raw,
                            off,
                            SCANOUT_PRIMARY_ZERO_PITCH.load(Ordering::Relaxed)
                        );
                        }
                        log_error!(
                        "DDI create_resource(tex2d): SCAN-OUT PRIMARY CREATE FAILED {}x{} fmt={} bind=0x{:x} -> no primary (optimal/dmabuf rejected?)",
                        mip0.TexelWidth, mip0.TexelHeight, a.Format, bind
                    );
                        // The loudness has to reach the runtime, not stop at the log
                        // file: this DDI returns void, so pfnSetErrorCb is the only
                        // way CreateTexture2D fails instead of handing the caller
                        // S_OK with a null driver resource. E_OUTOFMEMORY is in
                        // CreateTexture2D's documented return set; the bridge
                        // returns 0 with no HRESULT to map through.
                        set_runtime_error(h, E_OUTOFMEMORY);
                    }
                }
                handled = true;
            }
            if !handled {
                let mut tex: Option<ID3D11Texture2D> = None;
                // The five existing tex2d trace gates are the same predicate;
                // name it once so the restructured arm cannot drift between them.
                let big = mip0.TexelWidth >= 1024 || mip0.TexelHeight >= 576 || misc != 0;
                if big {
                    log_error!(
                        "DDI create_resource(tex2d): calling DXVK CreateTexture2D {}x{} fmt={} bind=0x{:x} misc=0x{:x} init={} hrt={:p} mips={} array={} usage={} cpu=0x{:x} sample={}x{}",
                        mip0.TexelWidth,
                        mip0.TexelHeight,
                        a.Format,
                        bind,
                        misc,
                        init_ptr.is_some(),
                        h_rt.handle,
                        desc.MipLevels,
                        desc.ArraySize,
                        a.Usage,
                        cpu,
                        a.SampleDesc.Count,
                        a.SampleDesc.Quality
                    );
                }
                let created = device.CreateTexture2D(&desc, init_ptr, Some(&mut tex));
                match created {
                    Ok(()) => {
                        if big {
                            log_error!(
                                "DDI create_resource(tex2d): DXVK CreateTexture2D returned S_OK tex_present={}",
                                tex.is_some()
                            );
                        }
                    }
                    Err(ref e) => log_error!("DDI create_resource(tex2d) failed: {e:?}"),
                }
                let res = match tex {
                    Some(t) => match t.cast::<ID3D11Resource>() {
                        Ok(r) => {
                            if big {
                                log_error!("DDI create_resource(tex2d): cast to ID3D11Resource OK");
                            }
                            Some(r)
                        }
                        Err(_) => {
                            if big {
                                log_error!(
                                    "DDI create_resource(tex2d): cast to ID3D11Resource failed"
                                );
                            }
                            None
                        }
                    },
                    None => {
                        if big && created.is_ok() {
                            log_error!(
                                "DDI create_resource(tex2d): DXVK CreateTexture2D returned no texture"
                            );
                        }
                        None
                    }
                };
                finish_create(h, created, res, |res| {
                    finish_wddm_tex2d(h, a, &mip0, h_rt, h_resource, res, false, None);
                });
            }
        }
        ResourceDimension::Texture1D => {
            // Same shape as the tex3d arm: create first, then allocate, then
            // store — no fallible step between the allocation and the store, so
            // it does not need R407's rollback. All four view translators
            // already handle RES_TEX1D; this arm is what stops
            // `CreateTexture1D` from being an outright failure now that the
            // catch-all reports.
            let bind = api_bind_flags(a.BindFlags);
            let misc = api_misc_flags(a.MiscFlags, a.BindFlags, false);
            log_error!(
                "DDI create_resource(tex1d): {} fmt={} usage={} bind 0x{:x}->0x{:x} misc 0x{:x}->0x{:x} init={} mips={} array={}",
                mip0.TexelWidth,
                a.Format,
                a.Usage,
                a.BindFlags,
                bind,
                a.MiscFlags,
                misc,
                init_ptr.is_some(),
                a.MipLevels,
                a.ArraySize
            );
            let desc = D3D11_TEXTURE1D_DESC {
                Width: mip0.TexelWidth,
                MipLevels: a.MipLevels,
                ArraySize: a.ArraySize.max(1),
                Format: DXGI_FORMAT(a.Format as i32),
                Usage: D3D11_USAGE(a.Usage as i32),
                BindFlags: bind,
                CPUAccessFlags: cpu,
                MiscFlags: misc,
            };
            let mut tex: Option<ID3D11Texture1D> = None;
            let created = device.CreateTexture1D(&desc, init_ptr, Some(&mut tex));
            if let Err(ref e) = created {
                log_error!("DDI create_resource(tex1d) failed: {e:?}");
            }
            let res = match tex {
                Some(t) => match t.cast::<ID3D11Resource>() {
                    Ok(r) => Some(r),
                    Err(_) => {
                        log_error!("DDI create_resource(tex1d): cast to ID3D11Resource failed");
                        None
                    }
                },
                None => {
                    if created.is_ok() {
                        log_error!(
                            "DDI create_resource(tex1d): DXVK CreateTexture1D returned no texture"
                        );
                    }
                    None
                }
            };
            finish_create(h, created, res, |res| {
                let (allocation, km_resource) = match allocate_wddm_resource(
                    h, a, &mip0, h_rt, None, false, None,
                ) {
                    Ok(allocation) => allocation,
                    Err(hr) => {
                        log_error!(
                                "DDI create_resource(tex1d): WDDM allocation/residency failed hr=0x{:08x}",
                                hr as u32
                            );
                        set_runtime_error(h, hr);
                        return;
                    }
                };
                let allocation_handle = allocation
                    .as_ref()
                    .map(ResidentAllocation::handle)
                    .unwrap_or(0);
                stamp_dxvk_resource_kmt_handles(h, &res, allocation_handle, km_resource);
                log_error!(
                    "DDI create_resource(tex1d) ok: {} fmt={} bind=0x{:x} misc=0x{:x}",
                    mip0.TexelWidth,
                    a.Format,
                    bind,
                    misc
                );
                store_resource(
                    h_resource,
                    res,
                    allocation,
                    km_resource,
                    h_rt.handle,
                    AllocationOwnership::CreatedByUmd,
                    empty_present_private(),
                    None,
                );
            });
        }
        ResourceDimension::Texture3D => {
            let bind = api_bind_flags(a.BindFlags);
            let misc = api_misc_flags(a.MiscFlags, a.BindFlags, false);
            log_error!(
                "DDI create_resource(tex3d): {}x{}x{} fmt={} usage={} bind 0x{:x}->0x{:x} misc 0x{:x}->0x{:x} init={} mips={}",
                mip0.TexelWidth,
                mip0.TexelHeight,
                mip0.TexelDepth,
                a.Format,
                a.Usage,
                a.BindFlags,
                bind,
                a.MiscFlags,
                misc,
                init_ptr.is_some(),
                a.MipLevels
            );
            let desc = D3D11_TEXTURE3D_DESC {
                Width: mip0.TexelWidth,
                Height: mip0.TexelHeight,
                Depth: mip0.TexelDepth.max(1),
                MipLevels: a.MipLevels,
                Format: DXGI_FORMAT(a.Format as i32),
                Usage: D3D11_USAGE(a.Usage as i32),
                BindFlags: bind,
                CPUAccessFlags: cpu,
                MiscFlags: misc,
            };
            let mut tex: Option<ID3D11Texture3D> = None;
            let created = device.CreateTexture3D(&desc, init_ptr, Some(&mut tex));
            if let Err(ref e) = created {
                log_error!("DDI create_resource(tex3d) failed: {e:?}");
            }
            let res = match tex {
                Some(t) => match t.cast::<ID3D11Resource>() {
                    Ok(r) => Some(r),
                    Err(_) => {
                        log_error!("DDI create_resource(tex3d): cast to ID3D11Resource failed");
                        None
                    }
                },
                None => {
                    if created.is_ok() {
                        log_error!(
                            "DDI create_resource(tex3d): DXVK CreateTexture3D returned no texture"
                        );
                    }
                    None
                }
            };
            finish_create(h, created, res, |res| {
                // The allocation-failure arm below already reported through
                // set_runtime_error; `return` leaves the closure, and this is
                // the last statement of create_resource, so that is the same
                // exit it was before.
                let (allocation, km_resource) = match allocate_wddm_resource(
                    h, a, &mip0, h_rt, None, false, None,
                ) {
                    Ok(allocation) => allocation,
                    Err(hr) => {
                        log_error!(
                                "DDI create_resource(tex3d): WDDM allocation/residency failed hr=0x{:08x}",
                                hr as u32
                            );
                        set_runtime_error(h, hr);
                        return;
                    }
                };
                let allocation_handle = allocation
                    .as_ref()
                    .map(ResidentAllocation::handle)
                    .unwrap_or(0);
                stamp_dxvk_resource_kmt_handles(h, &res, allocation_handle, km_resource);
                log_error!(
                    "DDI create_resource(tex3d) ok: {}x{}x{} fmt={} bind=0x{:x} misc=0x{:x}",
                    mip0.TexelWidth,
                    mip0.TexelHeight,
                    mip0.TexelDepth,
                    a.Format,
                    bind,
                    misc
                );
                store_resource(
                    h_resource,
                    res,
                    allocation,
                    km_resource,
                    h_rt.handle,
                    AllocationOwnership::CreatedByUmd,
                    empty_present_private(),
                    None,
                );
            });
        }
    }
}

pub(crate) unsafe extern "C" fn destroy_resource(h: Hdevice, h_resource: ddi::D3D10DDI_HRESOURCE) {
    release_resource(h, h_resource);
}

/// Log and classify ONE open-time HWA2 candidate buffer; returns the descriptor
/// when it is valid.
///
/// A free function rather than a closure inside `open_resource` on purpose: a
/// closure with an annotated `&mut` parameter is given one inferred region for
/// all of its call sites, which would hold `descriptor` borrowed across the whole
/// scan. Nothing here needs to capture.
///
/// The two counters are separate because the two failures are separate facts: a
/// buffer of the WRONG LENGTH is a producer/ABI mismatch (a 96-byte
/// pre-retirement record, or the oversized resource-level buffer), while a
/// 168-byte buffer that fails validation is a malformed or stale descriptor. The
/// pre-K4 reader returned `None` for both and for "nothing there", so the
/// refusal could not say which had happened.
fn note_open_candidate(
    result: Result<HeliosWddmAllocationDescV2, HeliosAllocDescRejection>,
    what: &str,
    ptr: *const c_void,
    size: u32,
    size_rejections: &mut usize,
    content_rejections: &mut usize,
) -> Option<HeliosWddmAllocationDescV2> {
    match result {
        Ok(desc) => {
            log_error!(
                "DDI open_resource {}: HWA2 alloc_gen=0x{:x} kind={} flags=0x{:x} bind=0x{:x} \
                 misc=0x{:x} size={} {}x{}x{} mips={} dxgi={} d3dddi={} samples={}x{} vidpn={} \
                 swizzle={} memory={} planes={} pitch={} plane_off={}",
                what,
                desc.allocation_generation,
                desc.allocation_kind,
                desc.flags,
                desc.bind_flags,
                desc.misc_flags,
                desc.byte_size,
                desc.width,
                desc.height,
                desc.depth_or_array_size,
                desc.mip_levels,
                desc.dxgi_format,
                desc.d3d_ddi_format,
                desc.sample_count,
                desc.sample_quality,
                desc.vidpn_source,
                desc.swizzle_class,
                desc.memory_class,
                desc.plane_count,
                desc.planes[0].row_pitch,
                desc.planes[0].offset
            );
            Some(desc)
        }
        Err(HeliosAllocDescRejection::PrivateDataSize { found, expected }) => {
            *size_rejections += 1;
            log_error!(
                "DDI open_resource {}: private={:p}/{} is not an HWA2 buffer (found {} bytes, \
                 need exactly {})",
                what,
                ptr,
                size,
                found,
                expected
            );
            None
        }
        Err(rejection) => {
            *content_rejections += 1;
            log_error!(
                "DDI open_resource {}: private={:p}/{} is a 168-byte buffer that FAILED HWA2 \
                 validation: {:?}",
                what,
                ptr,
                size,
                rejection
            );
            None
        }
    }
}

pub(crate) unsafe extern "C" fn open_resource(
    h: Hdevice,
    arg: *const ddi::D3D10DDIARG_OPENRESOURCE,
    h_resource: ddi::D3D10DDI_HRESOURCE,
    h_rt: ddi::D3D10DDI_HRTRESOURCE,
) {
    clear_handle(h_resource);

    if arg.is_null() {
        log_error!("DDI open_resource: null args");
        set_runtime_error(h, E_INVALIDARG);
        return;
    }

    let a = &*arg;
    let info2 = unsafe { a.__bindgen_anon_1.pOpenAllocationInfo2 };
    let mut allocation: ddi::D3DKMT_HANDLE = 0;

    if a.NumAllocations != 0 && info2.is_null() {
        log_error!("DDI open_resource FAILED: allocation array is null");
        set_runtime_error(h, E_INVALIDARG);
        return;
    }

    // ── the HWA2 open contract (K4) ─────────────────────────────────────────
    //
    // ⛔ What changed, and it is a change of MODEL, not of parser: the KMD used
    // to WRITE a `HeliosWddmOpenIdentity` over the first 48 bytes of the
    // open-time buffer in `DxgkDdiOpenAllocation`, so this driver waited for a
    // kernel write and two openers of one allocation could disagree about what
    // they had. `DxgkDdiOpenAllocation` now writes NO byte of the private
    // buffer (K4 acceptance obligation 5): the descriptor is written once at
    // create and is `const` for every opener. An opener VALIDATES AND READS.
    //
    // Every candidate buffer is parsed through
    // `HeliosWddmAllocationDescV2::from_private_data`, never a pointer cast, and
    // the length must be EXACTLY 168 — §10.3: a malformed, unknown, truncated
    // or mismatched-generation descriptor makes the open fail and "never selects
    // a legacy parser". The three outcomes are counted separately (wrong size /
    // invalid content / valid) because the old reader collapsed all of them into
    // `None` and the refusal could not say which had happened.
    let mut size_rejections = 0usize;
    let mut content_rejections = 0usize;
    // The resource-level buffer first, then each allocation's own. Order is a
    // preference, not a fallback chain that changes meaning: every candidate is
    // held to the identical validator.
    let mut descriptor = note_open_candidate(
        unsafe { read_open_descriptor(a.pPrivateDriverData, a.PrivateDriverDataSize) },
        "resource",
        a.pPrivateDriverData,
        a.PrivateDriverDataSize,
        &mut size_rejections,
        &mut content_rejections,
    );
    for index in 0..a.NumAllocations as usize {
        let info = &*info2.add(index);
        if index == 0 {
            allocation = info.hAllocation;
        }
        log_error!(
            "DDI open_resource allocation: index={} hDrv={:p} hRT={:p} hKM=0x{:x} \
             hAlloc=0x{:x} private={:p}/{} gpuva=0x{:x}",
            index,
            h_resource.pDrvPrivate,
            h_rt.handle,
            a.hKMResource.handle,
            info.hAllocation,
            info.pPrivateDriverData,
            info.PrivateDriverDataSize,
            info.GpuVirtualAddress,
        );
        let candidate = note_open_candidate(
            unsafe { read_open_descriptor(info.pPrivateDriverData, info.PrivateDriverDataSize) },
            "allocation",
            info.pPrivateDriverData,
            info.PrivateDriverDataSize,
            &mut size_rejections,
            &mut content_rejections,
        );
        // First valid candidate wins. Every allocation is still parsed and
        // logged, so a per-allocation disagreement stays visible instead of
        // being short-circuited away.
        if descriptor.is_none() {
            descriptor = candidate;
        }
    }

    // A shared open without a KMD descriptor cannot alias the real surface. The
    // old metadata-texture fallback fabricated a blank texture here and stamped
    // it with the real KMT handles — draws "succeeded" and the shared content
    // stayed black forever (audit U-B2). Fail loudly instead, and say WHICH of
    // the two failures happened.
    let Some(descriptor) = descriptor else {
        if content_rejections != 0 {
            note_ddi_refusal(&DDI_REFUSALS.hwa2_open_desc_invalid);
        } else {
            note_ddi_refusal(&DDI_REFUSALS.hwa2_open_private_size);
        }
        log_error!(
            "DDI open_resource FAILED: no valid HWA2 descriptor (hKM={:?} alloc=0x{:x} \
             num_alloc={} wrong_size={} invalid={}) -> E_FAIL",
            a.hKMResource,
            allocation,
            a.NumAllocations,
            size_rejections,
            content_rejections
        );
        set_runtime_error(h, E_FAIL);
        return;
    };

    if a.hKMResource.handle == 0 {
        log_error!(
            "DDI open_resource FAILED: no hKMResource (alloc_gen=0x{:x}) -> E_FAIL",
            descriptor.allocation_generation
        );
        set_runtime_error(h, E_FAIL);
        return;
    }

    // ⛔⛔ K4 / `K4-CONTRACT.md` §5 — the resid gap, at the consumer end.
    //
    // The descriptor above is valid and carries the full geometry: extent,
    // exact `DXGI_FORMAT`, bind/misc vocabulary, swizzle class, plane layout.
    // What it does not carry — and may never be extended to carry — is the host
    // resource id, the creating `vkAllocateMemory`'s size, and the Vulkan
    // memory-type index. `dxvk.open_texture2d` needs all three: they are how
    // the guest names the host object it is importing
    // (`VkImportMemoryResourceInfoMESA::resourceId`).
    //
    // §5 is explicit that re-pointing this reader at HWA2 is NOT a substitution
    // and must not be planned as one. The replacement is a different MECHANISM:
    // the ICD stops naming host resources at all and the KMD patches the resid
    // in from `HeliosNativeRenderPatch` — **mesa lane unit A3**, plus K6. Until
    // that lands, an ICD in this state CANNOT IMPORT, and §5 requires that to
    // be recorded as the retirement's intended intermediate state rather than a
    // regression.
    //
    // Refuse. Do not fall back, do not fabricate a 1x1 alias, do not reconstruct
    // an identity from the allocation handle: an alias that is UNDERSIZE
    // relative to the true allocation defeats even the oversize guard that
    // caught the 38th-session import regression.
    note_ddi_refusal(&DDI_REFUSALS.hwa2_open_needs_mesa_a3);
    log_error!(
        "DDI open_resource REFUSED: HWA2 carries no host resource id, venus allocation size \
         or memory-type index, and the D3D11 import needs all three (hKM={:?} alloc=0x{:x} \
         alloc_gen=0x{:x} {}x{} dxgi={} bind=0x{:x} misc=0x{:x} swizzle={} planes={}) -> \
         blocked on mesa unit A3 (+K6): the ICD stops naming host resources and the KMD \
         patches the resid in from HeliosNativeRenderPatch. An ICD in this state cannot \
         import; this is the retirement's intended intermediate state, not a regression.",
        a.hKMResource,
        allocation,
        descriptor.allocation_generation,
        descriptor.width,
        descriptor.height,
        descriptor.dxgi_format,
        descriptor.bind_flags,
        descriptor.misc_flags,
        descriptor.swizzle_class,
        descriptor.plane_count
    );
    set_runtime_error(h, E_FAIL);
}

pub(crate) unsafe extern "C" fn calc_size_opened_resource(
    _h: Hdevice,
    _arg: *const ddi::D3D10DDIARG_OPENRESOURCE,
) -> u64 {
    8
}

pub(crate) unsafe extern "C" fn resolve_shared_resource(
    h: ddi::HANDLE,
    arg: *const ddi::D3DDDIARG_RESOLVESHAREDRESOURCE,
) -> i32 {
    let h_resource: *mut c_void = if arg.is_null() {
        core::ptr::null_mut()
    } else {
        (*arg).hResource as *mut c_void
    };
    let resource = ddi::D3D10DDI_HRESOURCE {
        pDrvPrivate: h_resource,
    };
    let alloc = resource_allocation(resource);
    let (width, height) = resource_dimensions(resource);
    log_error!(
        "DDI ResolveSharedResource: hDevice={:p} hResource={:p} alloc=0x{:x} {}x{}",
        h,
        h_resource,
        alloc,
        width,
        height
    );
    if let Some(context) = d3d11_context(Hdevice {
        pDrvPrivate: h as *mut c_void,
    }) {
        context.Flush();
    }
    0
}

pub(crate) unsafe extern "C" fn dxgi_resolve_shared_resource(
    arg: *mut ddi::DXGI_DDI_ARG_RESOLVESHAREDRESOURCE,
) -> i32 {
    let (h_device, h_resource): (usize, usize) = if arg.is_null() {
        (0, 0)
    } else {
        ((*arg).hDevice as usize, (*arg).hResource as usize)
    };
    let resource = dxgi_resource_handle(h_resource as ddi::DXGI_DDI_HRESOURCE);
    let alloc = resource_allocation(resource);
    let (width, height) = resource_dimensions(resource);
    trace_line!(
        "DXGI ResolveSharedResource: hDevice=0x{:x} hResource=0x{:x} alloc=0x{:x} {}x{}",
        h_device,
        h_resource,
        alloc,
        width,
        height
    );
    if let Some(context) = d3d11_context(Hdevice {
        pDrvPrivate: h_device as *mut c_void,
    }) {
        context.Flush();
    }
    0
}
