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

/// The application's CPU view of an allocation.
///
/// ⛔ NOT a heap buffer. `CpuBacking` used to fill this role, and because it is
/// ordinary process memory the pointer handed to the application was a view of
/// NOTHING: the GPU worked on the allocation's venus blob while the application
/// read and wrote its own heap, which is why every GPU-routed read-back came
/// back untouched and the desktop was black.
///
/// Which callback publishes it is `UmdLockMode`; see
/// [`crate::knobs::UMD_LOCK_MODE`] for what each arm is and for the ETW
/// measurement that says mode 0 hands back a system-memory copy.
unsafe fn lock_cpu_view(
    kt_callbacks: *const ddi::D3DDDI_DEVICECALLBACKS,
    h_rt_device: ddi::HANDLE,
    h_allocation: u32,
) -> Result<*mut core::ffi::c_void, i32> {
    let mode = crate::knobs::umd_lock_mode();
    if mode == 2 {
        let Some(lock2_cb) = (*kt_callbacks).pfnLock2Cb else {
            log_error!("DDI lock_cpu_view: UmdLockMode=2 but pfnLock2Cb absent");
            return Err(E_OUTOFMEMORY);
        };
        let mut lock = ddi::D3DDDICB_LOCK2::default();
        lock.hAllocation = h_allocation;
        let hr = lock2_cb(h_rt_device, &mut lock);
        if hr != 0 || lock.pData.is_null() {
            log_error!(
                "DDI lock_cpu_view REFUSED (Lock2): hr=0x{:08x} alloc=0x{:x} pData={:p}",
                hr as u32,
                h_allocation,
                lock.pData
            );
            return Err(if hr != 0 { hr } else { E_OUTOFMEMORY });
        }
        return Ok(lock.pData);
    }

    let Some(lock_cb) = (*kt_callbacks).pfnLockCb else {
        log_error!("DDI lock_cpu_view: pfnLockCb absent");
        return Err(E_OUTOFMEMORY);
    };
    let mut lock = ddi::D3DDDICB_LOCK::default();
    lock.hAllocation = h_allocation;
    // Whole allocation: NumPages 0 with a null page list is the "entire
    // allocation" form, which is the only shape the aperture serves -- a blob
    // maps at one offset, whole-blob.
    lock.NumPages = 0;
    lock.pPages = core::ptr::null();
    if mode == 1 {
        // Ask VidMm to serve the lock where the allocation already is instead
        // of moving it to an aperture segment. A refusal here is the ANSWER,
        // not a regression: it says the local segment cannot serve a CPU lock
        // in place, which mode 0 hides behind a silent copy.
        lock.Flags.__bindgen_anon_1.__bindgen_anon_1.set_DonotEvict(1);
    }
    let hr = lock_cb(h_rt_device, &mut lock);
    if hr != 0 || lock.pData.is_null() {
        log_error!(
            "DDI lock_cpu_view REFUSED: mode={} hr=0x{:08x} alloc=0x{:x} pData={:p}",
            mode,
            hr as u32,
            h_allocation,
            lock.pData
        );
        return Err(if hr != 0 { hr } else { E_OUTOFMEMORY });
    }
    Ok(lock.pData)
}

fn resource_needs_cpu_mapping(
    a: &ddi::D3D11DDIARG_CREATERESOURCE,
    dimension: ResourceDimension,
) -> bool {
    const DDI_MISC_TILED: u32 = 0x0000_4000;
    const DDI_MISC_TILE_POOL: u32 = 0x0000_8000;
    const DDI_CPU_ACCESS_MASK: u32 = ddi::D3D10_DDI_CPU_ACCESS_D3D10_DDI_CPU_ACCESS_MASK as u32;
    const DDI_USAGE_DYNAMIC: u32 = ddi::D3D10_DDI_RESOURCE_USAGE_D3D10_DDI_USAGE_DYNAMIC as u32;
    const DDI_USAGE_STAGING: u32 = ddi::D3D10_DDI_RESOURCE_USAGE_D3D10_DDI_USAGE_STAGING as u32;
    const DDI_BIND_CONSTANT_BUFFER: u32 =
        ddi::D3D10_DDI_RESOURCE_BIND_FLAG_D3D10_DDI_BIND_CONSTANT_BUFFER as u32;

    if a.MiscFlags & (DDI_MISC_TILED | DDI_MISC_TILE_POOL) != 0 {
        return false;
    }
    const fn cpu_access_requested(map_flags: u32) -> bool {
        map_flags & DDI_CPU_ACCESS_MASK != 0
    }
    match dimension {
        ResourceDimension::Buffer => {
            cpu_access_requested(a.MapFlags)
                || a.Usage == DDI_USAGE_DYNAMIC
                || a.Usage == DDI_USAGE_STAGING
                || a.BindFlags & DDI_BIND_CONSTANT_BUFFER != 0
        }
        _ => cpu_access_requested(a.MapFlags),
    }
}

/// DXVK implements a staging texture as one mapped buffer per subresource and
/// deliberately creates no VkImage for it. Those buffers already enter the A5
/// record-only allocator independently, so each receives its own exact HRA1
/// identity. A top-level image preflight/allocation would both fail (there is
/// no image) and incorrectly collapse a multi-subresource texture onto one
/// outer token.
fn texture2d_uses_internal_mapped_buffers(usage: u32) -> bool {
    usage == D3D11_USAGE_STAGING.0 as u32
}

const WDDM_ALLOCATION_PAGE_BYTES: u64 = 4096;

fn round_up_wddm_allocation_bytes(bytes: u64) -> Option<u64> {
    bytes
        .checked_add(WDDM_ALLOCATION_PAGE_BYTES - 1)
        .map(|end| end & !(WDDM_ALLOCATION_PAGE_BYTES - 1))
}

fn select_wddm_allocation_bytes(
    resource_extent: u64,
    lower_memory_requirement: u64,
) -> Option<u64> {
    round_up_wddm_allocation_bytes(resource_extent.max(lower_memory_requirement))
}

/// Rebuild the D3D11 API descriptor for one runtime-opened HWA2 image.
///
/// HWA2's bind and misc words are package vocabularies, not D3D11 bitfields.
/// Keep the inverse translation here beside the create-time forward
/// translation in `alloc.rs`; in particular, HWA2 `PRESENT` has no D3D11 API
/// bind bit and must not be reinterpreted as `UNORDERED_ACCESS` merely because
/// both happen to occupy bit 8 in their respective vocabularies.
fn opened_texture2d_desc(descriptor: &HeliosWddmAllocationDescV2) -> Option<D3D11_TEXTURE2D_DESC> {
    if !descriptor.is_image() {
        return None;
    }

    use helios_protocol::{
        HELIOS_HWA2_BIND_CONSTANT_BUFFER, HELIOS_HWA2_BIND_DEPTH_STENCIL,
        HELIOS_HWA2_BIND_INDEX_BUFFER, HELIOS_HWA2_BIND_RENDER_TARGET,
        HELIOS_HWA2_BIND_SHADER_RESOURCE, HELIOS_HWA2_BIND_STREAM_OUTPUT,
        HELIOS_HWA2_BIND_UNORDERED_ACCESS, HELIOS_HWA2_BIND_VERTEX_BUFFER,
        HELIOS_HWA2_BIND_VIDEO_DECODER, HELIOS_HWA2_BIND_VIDEO_ENCODER, HELIOS_HWA2_FLAG_SHARED,
        HELIOS_HWA2_MISC_GDI_COMPATIBLE, HELIOS_HWA2_MISC_RESOURCE_CLAMP,
        HELIOS_HWA2_MISC_SHARED_NT_HANDLE, HELIOS_HWA2_MISC_TEXTURE_CUBE,
    };

    let mut bind = 0;
    for (hwa2_bit, d3d11_bit) in [
        (
            HELIOS_HWA2_BIND_VERTEX_BUFFER,
            D3D11_BIND_VERTEX_BUFFER.0 as u32,
        ),
        (
            HELIOS_HWA2_BIND_INDEX_BUFFER,
            D3D11_BIND_INDEX_BUFFER.0 as u32,
        ),
        (
            HELIOS_HWA2_BIND_CONSTANT_BUFFER,
            D3D11_BIND_CONSTANT_BUFFER.0 as u32,
        ),
        (
            HELIOS_HWA2_BIND_SHADER_RESOURCE,
            D3D11_BIND_SHADER_RESOURCE.0 as u32,
        ),
        (
            HELIOS_HWA2_BIND_STREAM_OUTPUT,
            D3D11_BIND_STREAM_OUTPUT.0 as u32,
        ),
        (
            HELIOS_HWA2_BIND_RENDER_TARGET,
            D3D11_BIND_RENDER_TARGET.0 as u32,
        ),
        (
            HELIOS_HWA2_BIND_DEPTH_STENCIL,
            D3D11_BIND_DEPTH_STENCIL.0 as u32,
        ),
        (
            HELIOS_HWA2_BIND_UNORDERED_ACCESS,
            D3D11_BIND_UNORDERED_ACCESS.0 as u32,
        ),
        (HELIOS_HWA2_BIND_VIDEO_DECODER, D3D11_BIND_DECODER.0 as u32),
        (
            HELIOS_HWA2_BIND_VIDEO_ENCODER,
            D3D11_BIND_VIDEO_ENCODER.0 as u32,
        ),
    ] {
        if descriptor.bind_flags & hwa2_bit != 0 {
            bind |= d3d11_bit;
        }
    }

    let mut misc = 0;
    for (hwa2_bit, d3d11_bit) in [
        (
            HELIOS_HWA2_MISC_GDI_COMPATIBLE,
            D3D11_RESOURCE_MISC_GDI_COMPATIBLE.0 as u32,
        ),
        (
            HELIOS_HWA2_MISC_TEXTURE_CUBE,
            D3D11_RESOURCE_MISC_TEXTURECUBE.0 as u32,
        ),
        (
            HELIOS_HWA2_MISC_RESOURCE_CLAMP,
            D3D11_RESOURCE_MISC_RESOURCE_CLAMP.0 as u32,
        ),
        (
            HELIOS_HWA2_MISC_SHARED_NT_HANDLE,
            D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0 as u32,
        ),
    ] {
        if descriptor.misc_flags & hwa2_bit != 0 {
            misc |= d3d11_bit;
        }
    }
    // The runtime opened a shared WDDM resource. Keep DXVK's resource model
    // shared as the pre-HRA1 open path did, while the HRA1 association below
    // suppresses every Vulkan external-memory import/export carrier.
    if descriptor.flags & HELIOS_HWA2_FLAG_SHARED != 0 {
        misc |= D3D11_RESOURCE_MISC_SHARED.0 as u32;
    }

    Some(D3D11_TEXTURE2D_DESC {
        Width: descriptor.width,
        Height: descriptor.height,
        MipLevels: descriptor.mip_levels,
        ArraySize: descriptor.depth_or_array_size,
        Format: DXGI_FORMAT(descriptor.dxgi_format as i32),
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: descriptor.sample_count,
            Quality: descriptor.sample_quality,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: bind,
        CPUAccessFlags: 0,
        MiscFlags: misc,
    })
}

#[cfg(test)]
mod allocation_size_tests {
    use super::*;

    #[test]
    fn staging_texture2d_bypasses_image_preflight() {
        assert!(texture2d_uses_internal_mapped_buffers(
            D3D11_USAGE_STAGING.0 as u32
        ));
        assert!(!texture2d_uses_internal_mapped_buffers(0));
        assert!(!texture2d_uses_internal_mapped_buffers(1));
        assert!(!texture2d_uses_internal_mapped_buffers(2));
    }

    #[test]
    fn wddm_allocation_extent_is_page_rounded() {
        assert_eq!(round_up_wddm_allocation_bytes(1), Some(4096));
        assert_eq!(round_up_wddm_allocation_bytes(4096), Some(4096));
        assert_eq!(round_up_wddm_allocation_bytes(12_800), Some(16_384));
        assert_eq!(round_up_wddm_allocation_bytes(u64::MAX), None);
    }

    #[test]
    fn lower_image_requirement_expands_the_outer_allocation() {
        assert_eq!(select_wddm_allocation_bytes(81_920, 131_072), Some(131_072));
        assert_eq!(select_wddm_allocation_bytes(131_072, 81_920), Some(131_072));
        assert_eq!(select_wddm_allocation_bytes(12_800, 16_384), Some(16_384));
    }
}

#[cfg(test)]
mod open_resource_tests {
    use super::*;
    use helios_protocol::{
        HELIOS_HWA2_BIND_PRESENT, HELIOS_HWA2_BIND_RENDER_TARGET, HELIOS_HWA2_BIND_SHADER_RESOURCE,
        HELIOS_HWA2_FLAG_RESOURCE_ASSOCIATED, HELIOS_HWA2_FLAG_SHARED, HELIOS_HWA2_KIND_BUFFER,
        HELIOS_HWA2_KIND_IMAGE, HELIOS_HWA2_MEMORY_SHARED, HELIOS_HWA2_MISC_GDI_COMPATIBLE,
        HELIOS_HWA2_MISC_RESOURCE_CLAMP, HELIOS_HWA2_SWIZZLE_LINEAR,
    };

    fn shared_image() -> HeliosWddmAllocationDescV2 {
        let mut descriptor = HeliosWddmAllocationDescV2::header(HELIOS_PACKAGE_GENERATION, 7);
        descriptor.byte_size = 2 * 1024 * 1024;
        descriptor.width = 704;
        descriptor.height = 704;
        descriptor.depth_or_array_size = 2;
        descriptor.mip_levels = 3;
        descriptor.dxgi_format = 87;
        descriptor.d3d_ddi_format = 21;
        descriptor.sample_count = 1;
        descriptor.allocation_kind = HELIOS_HWA2_KIND_IMAGE;
        descriptor.flags = HELIOS_HWA2_FLAG_SHARED | HELIOS_HWA2_FLAG_RESOURCE_ASSOCIATED;
        descriptor.bind_flags = HELIOS_HWA2_BIND_SHADER_RESOURCE
            | HELIOS_HWA2_BIND_RENDER_TARGET
            | HELIOS_HWA2_BIND_PRESENT;
        descriptor.misc_flags = HELIOS_HWA2_MISC_GDI_COMPATIBLE | HELIOS_HWA2_MISC_RESOURCE_CLAMP;
        descriptor.swizzle_class = HELIOS_HWA2_SWIZZLE_LINEAR;
        descriptor.memory_class = HELIOS_HWA2_MEMORY_SHARED;
        descriptor.plane_count = 1;
        descriptor.planes[0].row_pitch = 2816;
        descriptor.planes[0].slice_pitch = 1_982_464;
        assert_eq!(
            descriptor.validate_create_output(HELIOS_PACKAGE_GENERATION),
            Ok(())
        );
        descriptor
    }

    #[test]
    fn opened_image_preserves_exact_geometry_and_translates_vocabularies() {
        let descriptor = shared_image();
        let opened = opened_texture2d_desc(&descriptor).expect("image descriptor");

        assert_eq!(opened.Width, 704);
        assert_eq!(opened.Height, 704);
        assert_eq!(opened.ArraySize, 2);
        assert_eq!(opened.MipLevels, 3);
        assert_eq!(opened.Format.0, 87);
        assert_eq!(opened.SampleDesc.Count, 1);
        assert_eq!(opened.SampleDesc.Quality, 0);
        assert_eq!(opened.Usage, D3D11_USAGE_DEFAULT);
        assert_eq!(opened.CPUAccessFlags, 0);
        assert_eq!(
            opened.BindFlags,
            D3D11_BIND_SHADER_RESOURCE.0 as u32 | D3D11_BIND_RENDER_TARGET.0 as u32
        );
        assert_eq!(
            opened.MiscFlags,
            D3D11_RESOURCE_MISC_SHARED.0 as u32
                | D3D11_RESOURCE_MISC_GDI_COMPATIBLE.0 as u32
                | D3D11_RESOURCE_MISC_RESOURCE_CLAMP.0 as u32
        );
    }

    #[test]
    fn opened_non_image_is_not_reinterpreted_as_a_texture() {
        let mut descriptor = shared_image();
        descriptor.allocation_kind = HELIOS_HWA2_KIND_BUFFER;
        assert!(opened_texture2d_desc(&descriptor).is_none());
    }
}

/// Add one allocation to the WDDM 2.x device residency list.
///
/// E_PENDING is completed with a blocking monitored-fence wait through the
/// runtime callback. No command referencing the allocation may be submitted
/// before that fence reaches `PagingFenceValue`.
pub(crate) unsafe fn make_resident(
    outer: &crate::device_funcs::OuterDevice,
    handle: ddi::D3DKMT_HANDLE,
) -> Result<ResidentAllocation, i32> {
    const E_PENDING: i32 = 0x8000_000Au32 as i32;

    let Some(handle) = core::num::NonZeroU32::new(handle) else {
        log_error!("WDDM residency: zero allocation handle");
        return Err(E_INVALIDARG);
    };
    let Some(queue) = outer.paging_queue else {
        log_error!("WDDM residency: device has no paging queue");
        return Err(E_FAIL);
    };
    if outer.kt_callbacks.is_null() {
        log_error!("WDDM residency: no runtime callbacks");
        return Err(E_FAIL);
    }
    let Some(make_resident_cb) = (*outer.kt_callbacks).pfnMakeResidentCb else {
        log_error!("WDDM residency: pfnMakeResidentCb missing");
        return Err(E_FAIL);
    };
    let Some(evict_cb) = (*outer.kt_callbacks).pfnEvictCb else {
        // Do not acquire a residency reference that cannot be balanced.
        log_error!("WDDM residency: pfnEvictCb missing");
        return Err(E_FAIL);
    };

    let allocation = handle.get();
    let mut arg = ddi::D3DDDI_MAKERESIDENT::default();
    arg.hPagingQueue = queue.handle.get();
    arg.NumAllocations = 1;
    arg.AllocationList = &allocation;
    let hr = make_resident_cb(outer.h_rt_device, &mut arg);
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

    let resident = ResidentAllocation::new(handle, outer.h_rt_device, evict_cb);
    if hr == E_PENDING {
        let Some(wait_cb) = (*outer.kt_callbacks).pfnWaitForSynchronizationObjectFromCpuCb else {
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
        let wait_hr = wait_cb(outer.h_rt_device, &wait);
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
    outer: &crate::device_funcs::OuterDevice,
    allocation: ddi::D3DKMT_HANDLE,
) -> bool {
    if allocation == 0 {
        return true;
    }
    if outer.kt_callbacks.is_null() {
        return false;
    }
    let Some(deallocate_cb) = (*outer.kt_callbacks).pfnDeallocateCb else {
        return false;
    };
    let mut allocation = allocation;
    let mut arg = ddi::D3DDDICB_DEALLOCATE {
        hResource: core::ptr::null_mut(),
        NumAllocations: 1,
        HandleList: &mut allocation,
    };
    let hr = deallocate_cb(outer.h_rt_device, &mut arg);
    log_error!(
        "DDI allocate rollback: alloc=0x{:x} hr=0x{:08x}",
        allocation,
        hr as u32
    );
    hr == 0
}

fn finish_cpu_backing_rollback(
    cpu_backing: Option<helios_umd_common::cpu_backing::CpuBacking>,
    deallocated: bool,
) {
    if !deallocated {
        if let Some(backing) = cpu_backing {
            backing.leak();
        }
    }
}

pub(crate) struct CreatedWddmAllocation {
    resident: Option<ResidentAllocation>,
    km_resource: ddi::D3DKMT_HANDLE,
    identity: OuterAllocationIdentity,
    association: HeliosResourceAssociationV1,
    cpu_backing: Option<helios_umd_common::cpu_backing::CpuBacking>,
}

impl CreatedWddmAllocation {
    fn allocation_handle(&self) -> ddi::D3DKMT_HANDLE {
        self.resident
            .as_ref()
            .map(ResidentAllocation::handle)
            .unwrap_or(0)
    }

    fn association(&self) -> &HeliosResourceAssociationV1 {
        &self.association
    }

    unsafe fn rollback(mut self, dev: &crate::device_funcs::HeliosDevice) {
        let allocation = self.allocation_handle();
        let cpu_backing = self.cpu_backing.take();
        remove_outer_allocation(&dev.outer, self.identity);
        // Deallocate first, then release the residency guard through the
        // channel that actually ran -- see `state::release_residency`.
        let deallocated = deallocate_standalone(&dev.outer, allocation);
        release_residency(self.resident.take(), deallocated);
        finish_cpu_backing_rollback(cpu_backing, deallocated);
    }

    fn into_state(
        mut self,
    ) -> (
        Option<ResidentAllocation>,
        ddi::D3DKMT_HANDLE,
        Option<OuterAllocationIdentity>,
        Option<helios_umd_common::cpu_backing::CpuBacking>,
    ) {
        (
            self.resident.take(),
            self.km_resource,
            Some(self.identity),
            self.cpu_backing.take(),
        )
    }
}

/// One allocation handle dxgkrnl handed this device at `OpenResource`.
///
/// It participates in the same exact per-device HRA1 token map as a created
/// allocation, but rollback may only evict it.  The runtime owns the open and
/// therefore this UMD must never pass its handle to `pfnDeallocateCb`'s
/// HandleList form.
struct OpenedWddmAllocation {
    resident: Option<ResidentAllocation>,
    km_resource: ddi::D3DKMT_HANDLE,
    identity: OuterAllocationIdentity,
    association: HeliosResourceAssociationV1,
}

impl OpenedWddmAllocation {
    unsafe fn rollback(mut self, dev: &crate::device_funcs::HeliosDevice) {
        remove_outer_allocation(&dev.outer, self.identity);
        // The runtime owns the open, so nothing deallocates here and the evict
        // is the only way to give the residency reference back.
        release_residency(self.resident.take(), false);
    }

    fn into_state(
        mut self,
    ) -> (
        Option<ResidentAllocation>,
        ddi::D3DKMT_HANDLE,
        Option<OuterAllocationIdentity>,
    ) {
        (self.resident.take(), self.km_resource, Some(self.identity))
    }
}

/// The KMD did NOT import the offered pages (`HELIOS_HWA2_FLAG_GUEST_PAGE_BACKED`
/// missing after a successful create). The allocation was torn down again;
/// the backing comes back so the caller can hold it while it retries.
enum InternalAllocationError {
    Hr(i32),
    GuestBackingRefused(Option<helios_umd_common::cpu_backing::CpuBacking>),
}

/// How many fresh page sets to offer before falling back to the Lock view.
const GUEST_BACKING_RETRIES: u32 = 3;

/// Create the exact standalone WDDM allocation that backs one DXVK-internal
/// VkDeviceMemory, then retain its residency and ownership on this device.
/// The returned HRA1 value is consumed synchronously by that same
/// vkAllocateMemory call. No handle crosses into Mesa; an optional CPU data
/// view does, but is never used as identity or as a lookup key.
pub(crate) unsafe fn allocate_dxvk_internal_wddm_memory(
    outer: &crate::device_funcs::OuterDevice,
    bytes: u64,
    cpu_visible: bool,
    device_local: bool,
) -> Result<HeliosResourceAssociationV1, i32> {
    // Measured 2026-09-02: the KMD refuses a page set whose frames fragment
    // into more than the host udmabuf's 1024 runs (one 64 MiB chunk per boot),
    // and the old code then published those pages as the CPU view anyway —
    // two buffers, every readback from that chunk all-zero (black paintcap).
    // Holding the refused set while allocating the next one forces the heap
    // to hand out different pages.
    let mut refused: Vec<helios_umd_common::cpu_backing::CpuBacking> = Vec::new();
    for attempt in 0..=GUEST_BACKING_RETRIES {
        match allocate_dxvk_internal_wddm_memory_attempt(outer, bytes, cpu_visible, device_local, true) {
            Ok(association) => {
                if attempt != 0 {
                    log_error!(
                        "DXVK internal allocation: guest backing accepted on attempt {} bytes={}",
                        attempt + 1,
                        bytes
                    );
                }
                return Ok(association);
            }
            Err(InternalAllocationError::Hr(hr)) => return Err(hr),
            Err(InternalAllocationError::GuestBackingRefused(backing)) => {
                note_ddi_refusal(&DDI_REFUSALS.guest_backing_refused);
                log_error!(
                    "DXVK internal allocation: KMD refused the offered pages (attempt {} of {}) bytes={}",
                    attempt + 1,
                    GUEST_BACKING_RETRIES + 1,
                    bytes
                );
                refused.extend(backing);
            }
        }
    }
    note_ddi_refusal(&DDI_REFUSALS.guest_backing_fallback_lock);
    log_error!(
        "DXVK internal allocation: every page set refused, CPU view falls back to Lock bytes={}",
        bytes
    );
    drop(refused);
    match allocate_dxvk_internal_wddm_memory_attempt(outer, bytes, cpu_visible, device_local, false) {
        Ok(association) => Ok(association),
        Err(InternalAllocationError::Hr(hr)) => Err(hr),
        Err(InternalAllocationError::GuestBackingRefused(_)) => Err(E_FAIL),
    }
}

unsafe fn allocate_dxvk_internal_wddm_memory_attempt(
    outer: &crate::device_funcs::OuterDevice,
    bytes: u64,
    cpu_visible: bool,
    device_local: bool,
    offer_pages: bool,
) -> Result<HeliosResourceAssociationV1, InternalAllocationError> {
    use helios_protocol::{
        HELIOS_HWA2_FLAG_CPU_VISIBLE, HELIOS_HWA2_FLAG_KMD_OWNED_MASK, HELIOS_HWA2_KIND_BUFFER,
        HELIOS_HWA2_MEMORY_CPU_VISIBLE, HELIOS_HWA2_MEMORY_DEVICE_LOCAL, HELIOS_HWA2_MEMORY_SHARED,
        HELIOS_HWA2_SWIZZLE_LINEAR,
    };
    use InternalAllocationError::Hr;

    if bytes == 0 || outer.kt_callbacks.is_null() {
        return Err(Hr(E_INVALIDARG));
    }
    let Some(allocate_cb) = (*outer.kt_callbacks).pfnAllocateCb else {
        return Err(Hr(E_FAIL));
    };

    let mut desc = HeliosWddmAllocationDescV2::header(HELIOS_PACKAGE_GENERATION, 0);
    desc.byte_size = bytes;
    desc.allocation_kind = HELIOS_HWA2_KIND_BUFFER;
    desc.swizzle_class = HELIOS_HWA2_SWIZZLE_LINEAR;
    if cpu_visible {
        desc.flags = HELIOS_HWA2_FLAG_CPU_VISIBLE;
        desc.memory_class = HELIOS_HWA2_MEMORY_CPU_VISIBLE;
    } else if device_local {
        desc.memory_class = HELIOS_HWA2_MEMORY_DEVICE_LOCAL;
    } else {
        desc.memory_class = HELIOS_HWA2_MEMORY_SHARED;
    }
    if let Err(refusal) = desc.validate_create_input(HELIOS_PACKAGE_GENERATION) {
        log_error!(
            "DXVK internal allocation REFUSED: invalid HWA2 input {:?} bytes={} cpu_visible={} device_local={}",
            refusal,
            bytes,
            cpu_visible,
            device_local
        );
        return Err(Hr(E_INVALIDARG));
    }

    // `UmdGuestBacking`: OUR pages become the allocation's storage on both
    // sides — the KMD imports them as the venus memory, and they are what this
    // UMD publishes as `cpu_mapping`. Off: no buffer, and the CPU view comes
    // from `pfnLockCb`.
    let mut cpu_backing = if offer_pages && crate::knobs::umd_guest_backing() == 1 && cpu_visible {
        let Some(backing) = helios_umd_common::cpu_backing::CpuBacking::new_page_rounded(bytes)
        else {
            log_error!("DXVK internal allocation REFUSED: no CPU backing for {} bytes", bytes);
            return Err(Hr(E_OUTOFMEMORY));
        };
        // ⭐ POSITIVE CONTROL on the page sharing itself, written BEFORE the
        // allocation exists and never touched again. EVERY page names its own
        // index, so any guest-physical address QEMU can be pointed at either
        // identifies itself as this buffer or does not — which the application's
        // own stamps cannot do, because DXVK suballocates and the map offset is
        // not knowable from outside. If the guest blob's first GPA reads
        // `0xB00B0000`, the pages the KMD locked ARE this buffer.
        // SAFETY: `backing` owns `bytes()` writable bytes; every write below is
        // the first dword of a page strictly inside that range.
        unsafe {
            let base = backing.as_ptr() as *mut u8;
            let pages = backing.bytes() / 4096;
            for page in 0..pages {
                core::ptr::write_unaligned(
                    base.add(page * 4096) as *mut u32,
                    0xB00B_0000 | (page as u32 & 0xFFFF),
                );
            }
        }
        Some(backing)
    } else {
        None
    };
    // The offer rides the RESOURCE-level buffer. It may NOT go in `desc`: that
    // record is echoed, and any nonzero tail byte in it makes dxgkrnl drop the
    // KMD's create-output write-back (isolated with a fake-value control on
    // 22.22.411.0), after which the allocation fails this UMD's own output
    // check with `AllocationGenerationZero`.
    let mut cpu_backing_record = cpu_backing.as_ref().map(|b| {
        helios_protocol::HeliosCpuBackingV1::new(b.as_ptr() as u64, b.bytes() as u64)
    });

    let sent = desc;
    let private_ptr = (&mut desc as *mut HeliosWddmAllocationDescV2).cast();
    let private_size = u32::from(HELIOS_HWA2_BYTES);
    let mut allocation_info = ddi::D3DDDI_ALLOCATIONINFO2::default();
    allocation_info.pPrivateDriverData = private_ptr;
    allocation_info.PrivateDriverDataSize = private_size;
    // ⛔ pSystemMem stays NULL even under `UmdGuestBacking`. Supplying it made
    // dxgkrnl refuse the whole transaction with E_INVALIDARG (measured,
    // 22.22.404.0): a pSystemMem allocation is system memory by definition and
    // cannot also prefer a video-memory segment. It buys nothing here either —
    // its only job would be to make `pfnLockCb` return this buffer, and this
    // mode publishes the pointer directly instead of asking.
    allocation_info.__bindgen_anon_1.pSystemMem = core::ptr::null();
    let mut alloc = ddi::D3DDDICB_ALLOCATE::default();
    // ⛔ It must NOT alias the allocation's, which is what it used to do.
    // Either the CPU-backing offer or nothing.
    match cpu_backing_record.as_mut() {
        Some(record) => {
            alloc.pPrivateDriverData = (record as *mut _ as *mut core::ffi::c_void).cast();
            alloc.PrivateDriverDataSize =
                u32::from(helios_protocol::HELIOS_CPU_BACKING_BYTES);
        }
        None => {
            alloc.pPrivateDriverData = core::ptr::null_mut();
            alloc.PrivateDriverDataSize = 0;
        }
    }
    alloc.hResource = core::ptr::null_mut();
    alloc.NumAllocations = 1;
    alloc.__bindgen_anon_1.pAllocationInfo2 = &mut allocation_info;

    let hr = allocate_cb(outer.h_rt_device, &mut alloc);
    let h_allocation = allocation_info.hAllocation;
    if hr != 0 || h_allocation == 0 {
        log_error!(
            "DXVK internal pfnAllocateCb REFUSED: hr=0x{:08x} alloc=0x{:x} bytes={}",
            hr as u32,
            h_allocation,
            bytes
        );
        let deallocated = h_allocation == 0 || deallocate_standalone(outer, h_allocation);
        finish_cpu_backing_rollback(cpu_backing.take(), deallocated);
        return Err(Hr(if hr != 0 { hr } else { E_OUTOFMEMORY }));
    }

    // Did OUR buffer survive the create, and is the VA we sent the one the KMD
    // locked? `GbInVaLo` publishes the KMD's view of the same address, so the
    // two together separate "the UMD sent the wrong VA" from "the MDL over the
    // right VA does not describe these pages".
    if let Some(record) = cpu_backing_record {
        // SAFETY: the buffer is alive until `cpu_backing` is dropped, which is
        // after every use below.
        let head = unsafe { core::ptr::read_unaligned(record.va as *const u32) };
        log_error!(
            "DDI gb-witness: alloc=0x{:x} va=0x{:x} bytes={} head=0x{:08x}",
            h_allocation,
            record.va,
            record.bytes,
            head
        );
    }

    // The create-output write-back, from the pointer dxgkrnl was given. Kept:
    // it is what proved the write-back exists at all and what will show it
    // stopping again. `sent_gen` is always 0 — the generation is the KMD's to
    // assign — so a nonzero `ptr_gen` IS the write-back having landed.
    // SAFETY: `private_ptr` is `&mut desc` cast; the record is `Copy` and
    // `private_size` bytes are valid for reads.
    let returned = unsafe {
        core::ptr::read_unaligned(private_ptr as *const HeliosWddmAllocationDescV2)
    };
    if returned.allocation_generation == 0 {
        log_error!(
            "DDI write-back MISSING: alloc=0x{:x} sent_gen={} local_gen={} ptr_gen={} size={}",
            h_allocation,
            sent.allocation_generation,
            desc.allocation_generation,
            returned.allocation_generation,
            private_size
        );
    }
    if let Err(refusal) = desc.validate_create_output(HELIOS_PACKAGE_GENERATION) {
        log_error!(
            "DXVK internal allocation REFUSED: invalid HWA2 output {:?} alloc=0x{:x}",
            refusal,
            h_allocation
        );
        let deallocated = deallocate_standalone(outer, h_allocation);
        finish_cpu_backing_rollback(cpu_backing.take(), deallocated);
        return Err(Hr(E_OUTOFMEMORY));
    }
    let echoed = HeliosWddmAllocationDescV2 {
        allocation_generation: sent.allocation_generation,
        flags: desc.flags & !HELIOS_HWA2_FLAG_KMD_OWNED_MASK,
        ..desc
    };
    if echoed != sent {
        log_error!(
            "DXVK internal allocation REFUSED: KMD changed non-owned HWA2 fields alloc=0x{:x}",
            h_allocation
        );
        let deallocated = deallocate_standalone(outer, h_allocation);
        finish_cpu_backing_rollback(cpu_backing.take(), deallocated);
        return Err(Hr(E_OUTOFMEMORY));
    }

    // REPORT, not echo (KMD create_allocation): the flag is set only when the
    // KMD really imported our pages. Without it this allocation is a host blob
    // and our pages are nobody's memory — never publish them as its CPU view.
    if cpu_backing.is_some()
        && !desc.has_flag(helios_protocol::HELIOS_HWA2_FLAG_GUEST_PAGE_BACKED)
    {
        let deallocated = deallocate_standalone(outer, h_allocation);
        if !deallocated {
            log_error!(
                "DXVK internal allocation: deallocate after refused guest backing FAILED alloc=0x{:x}",
                h_allocation
            );
            finish_cpu_backing_rollback(cpu_backing.take(), false);
            return Err(Hr(E_OUTOFMEMORY));
        }
        // The KMD never locked these pages (every refusal path unlocks), so
        // they are ours to hold or free.
        return Err(InternalAllocationError::GuestBackingRefused(cpu_backing.take()));
    }

    let resident = match make_resident(outer, h_allocation) {
        Ok(resident) => resident,
        Err(resident_hr) => {
            let deallocated = deallocate_standalone(outer, h_allocation);
            finish_cpu_backing_rollback(cpu_backing.take(), deallocated);
            return Err(Hr(resident_hr));
        }
    };
    // After residency: locking asks the KMD to populate the CPU host aperture
    // for this allocation, which it can only do once the allocation is placed.
    let cpu_mapping = match (cpu_visible, cpu_backing.as_ref()) {
        (false, _) => core::ptr::null_mut(),
        // Our own pages, which the KMD imported as this allocation's venus
        // memory. Publishing them is what makes the app's pointer and the GPU's
        // memory one buffer.
        (true, Some(backing)) => backing.as_ptr(),
        (true, None) => {
            match lock_cpu_view(outer.kt_callbacks, outer.h_rt_device, h_allocation) {
                Ok(ptr) => ptr,
                Err(lock_hr) => {
                    drop(resident);
                    let deallocated = deallocate_standalone(outer, h_allocation);
                    finish_cpu_backing_rollback(cpu_backing.take(), deallocated);
                    return Err(Hr(lock_hr));
                }
            }
        }
    };
    let (identity, association) = match assign_outer_allocation(
        outer,
        h_allocation,
        desc.allocation_generation,
        desc.byte_size,
        cpu_mapping,
        desc.has_flag(helios_protocol::HELIOS_HWA2_FLAG_GUEST_PAGE_BACKED),
    ) {
        Ok(assigned) => assigned,
        Err(refusal) => {
            log_error!(
                "DXVK internal allocation REFUSED: token assignment {:?} alloc=0x{:x}",
                refusal,
                h_allocation
            );
            drop(resident);
            let deallocated = deallocate_standalone(outer, h_allocation);
            finish_cpu_backing_rollback(cpu_backing.take(), deallocated);
            return Err(Hr(E_OUTOFMEMORY));
        }
    };
    if let Err((refusal, resident, cpu_backing)) =
        retain_internal_outer_allocation(outer, identity, resident, cpu_backing.take())
    {
        log_error!(
            "DXVK internal allocation REFUSED: ownership retention {:?} alloc=0x{:x}",
            refusal,
            h_allocation
        );
        remove_outer_allocation(outer, identity);
        drop(resident);
        let deallocated = deallocate_standalone(outer, h_allocation);
        finish_cpu_backing_rollback(cpu_backing, deallocated);
        return Err(Hr(E_OUTOFMEMORY));
    }
    Ok(association)
}

pub(crate) unsafe fn allocate_wddm_resource(
    h: Hdevice,
    a: &ddi::D3D11DDIARG_CREATERESOURCE,
    mip0: &ddi::D3D10DDI_MIPINFO,
    h_rt: ddi::D3D10DDI_HRTRESOURCE,
    lower_memory_requirement: u64,
) -> Result<CreatedWddmAllocation, i32> {
    const DDI_BIND_PRESENT: u32 = 0x0000_0080;

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

    const CROSS_ADAPTER_PITCH_ALIGN: u32 = 256;
    let raw_pitch = mip0
        .TexelWidth
        .saturating_mul(dxgi_bytes_per_pixel(a.Format as u32));
    let pitch =
        raw_pitch.saturating_add(CROSS_ADAPTER_PITCH_ALIGN - 1) & !(CROSS_ADAPTER_PITCH_ALIGN - 1);
    // The extent HWA2 records is the one this UMD can compute.
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
    // extent this driver can actually compute is the right baseline to send.
    // WAS `pitch * mip0.TexelHeight` — one 2D slice of mip 0 — while the
    // descriptor declares depth, array size and mips, and the KMD sizes the venus
    // blob FROM this number: a 64^3 shared Texture3D got 16 KiB for 1 MiB, the
    // Xid-31 undersize shape. Over-estimates on purpose (Tier 2 admits it); the
    // mip chain is bounded by 2x the base level. The final extent is rounded to
    // the WDDM page boundary. For an associated lower image, DXVK first creates
    // the exact storage-less image and retains it across outer allocation; that
    // same object is adopted for the later dedicated memory allocation. Its
    // measured size is `lower_memory_requirement`, and the outer allocation
    // must cover the larger bound. This is not an empirical padding rule: the
    // target's 256x80 BGRA image measured 131,072 bytes while its texel extent
    // was 81,920 bytes, and the smaller HRA1 was rejected locally before any
    // HOB1 could reach KMD.
    let slices = (mip0.TexelDepth.max(1) as u64).saturating_mul(a.ArraySize.max(1) as u64);
    let unaligned_size = (pitch as u64)
        .saturating_mul(mip0.TexelHeight.max(1) as u64)
        .saturating_mul(slices)
        .saturating_mul(if a.MipLevels > 1 { 2 } else { 1 })
        .max(WDDM_ALLOCATION_PAGE_BYTES);
    let size = select_wddm_allocation_bytes(unaligned_size, lower_memory_requirement)
        .ok_or(E_OUTOFMEMORY)?;

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
    let needs_cpu_mapping = resource_needs_cpu_mapping(a, dimension);

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
        plane_offset: 0,
        primary_vidpn_source,
        // THE DIRECT ARM (owner decision 2026-08-23). Every WDDM primary is what
        // `SET_SCANOUT_BLOB` binds, so it claims `DISPLAYABLE` and is built
        // `OPAQUE_OPTIMAL` for the QEMU fork's native reconstruction -- rather
        // than being copied into the KMD-owned LINEAR target. This was the
        // hard-coded `false` that made `HELIOS_HWA2_FLAG_DISPLAYABLE`
        // unreachable for every UMD allocation, so `SetVidPnSourceAddress`
        // refused DWM's primary 32x/boot (`D2AdmWhy=0x53`, KMD 22.22.341.0) and
        // the only thing ever scanned out was the KMD's own blank primary.
        direct_scanout_primary: primary_vidpn_source.is_some(),
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

    // `UmdGuestBacking`: OUR pages back the allocation on both sides. See the
    // identical block in `allocate_dxvk_internal_wddm_memory`.
    let mut cpu_backing = if crate::knobs::umd_guest_backing() == 1 && needs_cpu_mapping {
        let Some(backing) =
            helios_umd_common::cpu_backing::CpuBacking::new_page_rounded(desc.byte_size)
        else {
            log_error!(
                "DDI allocate_wddm_resource REFUSED: no CPU backing for {} bytes",
                desc.byte_size
            );
            return Err(E_OUTOFMEMORY);
        };
        Some(backing)
    } else {
        None
    };
    // Resource-level channel, not the echoed descriptor. See the sibling site.
    let mut cpu_backing_record = cpu_backing.as_ref().map(|b| {
        helios_protocol::HeliosCpuBackingV1::new(b.as_ptr() as u64, b.bytes() as u64)
    });

    let mut allocation_info = ddi::D3DDDI_ALLOCATIONINFO2::default();
    let private_ptr = (&mut desc as *mut HeliosWddmAllocationDescV2).cast();
    // ⛔ Exactly `HELIOS_HWA2_BYTES`. `from_private_data` requires the length to
    // be exact on the far side (`==`, not `>=`), so 96 -> 168 is a hard ABI
    // step, not a growth an older KMD tolerates.
    let private_size = u32::from(HELIOS_HWA2_BYTES);
    allocation_info.pPrivateDriverData = private_ptr;
    allocation_info.PrivateDriverDataSize = private_size;
    // ⛔ pSystemMem stays NULL even under `UmdGuestBacking`. Supplying it made
    // dxgkrnl refuse the whole transaction with E_INVALIDARG (measured,
    // 22.22.404.0): a pSystemMem allocation is system memory by definition and
    // cannot also prefer a video-memory segment. It buys nothing here either —
    // its only job would be to make `pfnLockCb` return this buffer, and this
    // mode publishes the pointer directly instead of asking.
    allocation_info.__bindgen_anon_1.pSystemMem = core::ptr::null();
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
    // ⛔ NOT `private_ptr`: aliasing the allocation's record here gave dxgkrnl
    // two copy-backs into one buffer. Either the CPU-backing offer or nothing.
    match cpu_backing_record.as_mut() {
        Some(record) => {
            alloc.pPrivateDriverData = (record as *mut _ as *mut core::ffi::c_void).cast();
            alloc.PrivateDriverDataSize =
                u32::from(helios_protocol::HELIOS_CPU_BACKING_BYTES);
        }
        None => {
            alloc.pPrivateDriverData = core::ptr::null_mut();
            alloc.PrivateDriverDataSize = 0;
        }
    }
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
        let deallocated =
            h_allocation == 0 || unsafe { deallocate_standalone(&dev.outer, h_allocation) };
        finish_cpu_backing_rollback(cpu_backing.take(), deallocated);
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
        let deallocated = unsafe { deallocate_standalone(&dev.outer, h_allocation) };
        finish_cpu_backing_rollback(cpu_backing.take(), deallocated);
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

    // Same rule as the internal path: the KMD reports whether it imported our
    // pages; refused pages are not this allocation's memory, so the CPU view
    // comes from Lock instead. (No retry here — resource-sized page sets
    // rarely exceed the host's 1024-run udmabuf limit.)
    if cpu_backing.is_some()
        && !desc.has_flag(helios_protocol::HELIOS_HWA2_FLAG_GUEST_PAGE_BACKED)
    {
        note_ddi_refusal(&DDI_REFUSALS.guest_backing_refused);
        note_ddi_refusal(&DDI_REFUSALS.guest_backing_fallback_lock);
        log_error!(
            "DDI allocate_wddm_resource: KMD refused the offered pages alloc=0x{:x} bytes={} -> Lock view",
            h_allocation,
            desc.byte_size
        );
        drop(cpu_backing.take());
    }

    match unsafe { make_resident(&dev.outer, h_allocation) } {
        Ok(resident) => match (match (needs_cpu_mapping, cpu_backing.as_ref()) {
            (false, _) => Ok(core::ptr::null_mut()),
            // Our own pages, imported by the KMD as this allocation's venus
            // memory: one buffer, not two.
            (true, Some(backing)) => Ok(backing.as_ptr()),
            (true, None) => unsafe {
                lock_cpu_view(dev.outer.kt_callbacks, dev.outer.h_rt_device, h_allocation)
            },
        })
        .and_then(|cpu_mapping| {
            assign_outer_allocation(
                &dev.outer,
                h_allocation,
                desc.allocation_generation,
                desc.byte_size,
                cpu_mapping,
                desc.has_flag(helios_protocol::HELIOS_HWA2_FLAG_GUEST_PAGE_BACKED),
            )
            .map_err(|refusal| {
                log_error!(
                    "DDI allocate_wddm_resource REFUSED: outer association {:?} alloc=0x{:x} generation={} bytes={}",
                    refusal,
                    h_allocation,
                    desc.allocation_generation,
                    desc.byte_size
                );
                E_OUTOFMEMORY
            })
        }) {
            Ok((identity, association)) => Ok(CreatedWddmAllocation {
                resident: Some(resident),
                km_resource: alloc.hKMResource,
                identity,
                association,
                cpu_backing,
            }),
            Err(hr) => {
                drop(resident);
                let deallocated = unsafe { deallocate_standalone(&dev.outer, h_allocation) };
                finish_cpu_backing_rollback(cpu_backing.take(), deallocated);
                Err(hr)
            }
        },
        Err(resident_hr) => {
            // pfnAllocateCb succeeded, so this UMD owns the allocation even
            // though residency failed. Roll it back before surfacing the
            // failure; no partially initialized ResourceState is created.
            let deallocated = unsafe { deallocate_standalone(&dev.outer, h_allocation) };
            finish_cpu_backing_rollback(cpu_backing.take(), deallocated);
            Err(resident_hr)
        }
    }
}

unsafe fn create_and_store_associated_resource(
    h: Hdevice,
    h_resource: ddi::D3D10DDI_HRESOURCE,
    h_rt: ddi::D3D10DDI_HRTRESOURCE,
    kind: u32,
    desc_ptr: usize,
    initial_data_ptr: usize,
    allocation: CreatedWddmAllocation,
    texture2d_preflight: Option<crate::bridge::AssociatedTexture2DPreflight>,
) -> Result<(), i32> {
    let Some(dev) = helios_device(h) else {
        return Err(E_FAIL);
    };
    let allocation_handle = allocation.allocation_handle();
    let association = *allocation.association();
    let Some(resource) = dev.dxvk.create_associated_resource(
        kind,
        desc_ptr,
        initial_data_ptr,
        &association,
        texture2d_preflight,
    ) else {
        allocation.rollback(dev);
        log_error!(
            "DDI associated create REFUSED: kind={} token={} allocation=0x{:x}",
            kind,
            association.outer_allocation_token,
            allocation_handle
        );
        return Err(E_OUTOFMEMORY);
    };
    let (resident, km_resource, outer_allocation, cpu_backing) = allocation.into_state();
    store_resource(
        h_resource,
        resource,
        resident,
        outer_allocation,
        cpu_backing,
        km_resource,
        h_rt.handle,
        AllocationOwnership::CreatedByUmd,
    );
    Ok(())
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
            let Some(dev) = helios_device(h) else {
                set_runtime_error(h, E_FAIL);
                return;
            };
            // The venus import model demands the buffer's exact lower memory
            // requirement (64 KiB for a 48-byte buffer, measured 2026-08-24);
            // sizing from ByteWidth alone made every dedicated import refuse.
            let lower_memory_requirement = unsafe {
                dev.dxvk.prepare_associated_buffer_bytes(
                    (&desc as *const D3D11_BUFFER_DESC) as usize,
                )
            };
            if lower_memory_requirement == 0 {
                log_error!("DDI create_resource(buffer): lower-buffer preflight failed");
                set_runtime_error(h, E_OUTOFMEMORY);
                return;
            }
            let allocation =
                match allocate_wddm_resource(h, a, &mip0, h_rt, lower_memory_requirement) {
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
            if let Err(hr) = create_and_store_associated_resource(
                h,
                h_resource,
                h_rt,
                0,
                (&desc as *const D3D11_BUFFER_DESC) as usize,
                init_ptr.map_or(0, |ptr| ptr as usize),
                allocation,
                None,
            ) {
                set_runtime_error(h, hr);
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
            if texture2d_uses_internal_mapped_buffers(a.Usage) {
                let mut texture = None;
                if let Err(e) = device.CreateTexture2D(&desc, init_ptr, Some(&mut texture)) {
                    log_error!(
                        "DDI create_resource(tex2d): staging mapped-buffer create failed {e:?}"
                    );
                    set_runtime_error(h, create_error_hr(&e));
                    return;
                }
                let Some(texture) = texture else {
                    log_error!(
                        "DDI create_resource(tex2d): staging mapped-buffer create returned no resource"
                    );
                    set_runtime_error(h, E_OUTOFMEMORY);
                    return;
                };
                let Ok(resource) = texture.cast::<ID3D11Resource>() else {
                    log_error!(
                        "DDI create_resource(tex2d): staging mapped-buffer resource cast failed"
                    );
                    set_runtime_error(h, E_OUTOFMEMORY);
                    return;
                };
                // The DDI resource has no single top-level WDDM allocation.
                // Each DXVK mapped buffer owns and retires its exact standalone
                // allocation through the existing A5 outer callbacks.
                store_resource(
                    h_resource,
                    resource,
                    None,
                    None,
                    None,
                    0,
                    h_rt.handle,
                    AllocationOwnership::CreatedByUmd,
                );
                return;
            }
            let Some(dev) = helios_device(h) else {
                set_runtime_error(h, E_FAIL);
                return;
            };
            let preflight = match unsafe {
                dev.dxvk
                    .prepare_associated_texture2d((&desc as *const D3D11_TEXTURE2D_DESC) as usize)
            } {
                Some(preflight) => preflight,
                None => {
                    log_error!("DDI create_resource(tex2d): exact lower-image preflight failed");
                    set_runtime_error(h, E_OUTOFMEMORY);
                    return;
                }
            };
            let lower_memory_requirement = preflight.bytes;
            let allocation =
                match allocate_wddm_resource(h, a, &mip0, h_rt, lower_memory_requirement) {
                    Ok(allocation) => allocation,
                    Err(hr) => {
                        dev.dxvk.discard_associated_texture2d_preflight(preflight);
                        log_error!(
                        "DDI create_resource(tex2d): WDDM allocation/residency failed hr=0x{:08x}",
                        hr as u32
                    );
                        set_runtime_error(h, hr);
                        return;
                    }
                };
            if let Err(hr) = create_and_store_associated_resource(
                h,
                h_resource,
                h_rt,
                2,
                (&desc as *const D3D11_TEXTURE2D_DESC) as usize,
                init_ptr.map_or(0, |ptr| ptr as usize),
                allocation,
                Some(preflight),
            ) {
                set_runtime_error(h, hr);
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
            let allocation = match allocate_wddm_resource(h, a, &mip0, h_rt, 0) {
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
            if let Err(hr) = create_and_store_associated_resource(
                h,
                h_resource,
                h_rt,
                1,
                (&desc as *const D3D11_TEXTURE1D_DESC) as usize,
                init_ptr.map_or(0, |ptr| ptr as usize),
                allocation,
                None,
            ) {
                set_runtime_error(h, hr);
            }
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
            let allocation = match allocate_wddm_resource(h, a, &mip0, h_rt, 0) {
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
            if let Err(hr) = create_and_store_associated_resource(
                h,
                h_resource,
                h_rt,
                3,
                (&desc as *const D3D11_TEXTURE3D_DESC) as usize,
                init_ptr.map_or(0, |ptr| ptr as usize),
                allocation,
                None,
            ) {
                set_runtime_error(h, hr);
            }
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

    // A7 binds one exact VkDeviceMemory to one exact outer allocation. HWA2
    // represents multiple image planes inside that allocation; it does not
    // authorize a disjoint multi-allocation resource or a guessed primary
    // allocation from an array.
    if a.NumAllocations != 1 || info2.is_null() {
        note_ddi_refusal(&DDI_REFUSALS.hwa2_open_unsupported_shape);
        log_error!(
            "DDI open_resource FAILED: expected one exact allocation, got {} array={:p}",
            a.NumAllocations,
            info2
        );
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

    let Some(dev) = helios_device(h) else {
        log_error!("DDI open_resource FAILED: no Helios device");
        set_runtime_error(h, E_FAIL);
        return;
    };
    let Some(open_desc) = opened_texture2d_desc(&descriptor) else {
        note_ddi_refusal(&DDI_REFUSALS.hwa2_open_unsupported_shape);
        log_error!(
            "DDI open_resource REFUSED: valid HWA2 kind {} is not a D3D11 shared image",
            descriptor.allocation_kind
        );
        set_runtime_error(h, E_FAIL);
        return;
    };

    // The lower image object is created once and retained across the WDDM
    // residency/token edge. This is the same exact-object preflight used by
    // CreateResource; recreating it after the callback reintroduces the
    // VkImage lifetime/object-number race seen during cold admission.
    let Some(preflight) = dev
        .dxvk
        .prepare_associated_texture2d((&open_desc as *const D3D11_TEXTURE2D_DESC) as usize)
    else {
        log_error!("DDI open_resource FAILED: exact lower-image preflight failed");
        set_runtime_error(h, E_OUTOFMEMORY);
        return;
    };
    let lower_memory_requirement = preflight.bytes;
    if lower_memory_requirement > descriptor.byte_size {
        dev.dxvk.discard_associated_texture2d_preflight(preflight);
        note_ddi_refusal(&DDI_REFUSALS.hwa2_open_unsupported_shape);
        log_error!(
            "DDI open_resource REFUSED: lower image needs {} bytes but HWA2 owns {} bytes \
             (alloc=0x{:x} generation={})",
            lower_memory_requirement,
            descriptor.byte_size,
            allocation,
            descriptor.allocation_generation
        );
        set_runtime_error(h, E_FAIL);
        return;
    }

    let resident = match make_resident(&dev.outer, allocation) {
        Ok(resident) => resident,
        Err(hr) => {
            dev.dxvk.discard_associated_texture2d_preflight(preflight);
            log_error!(
                "DDI open_resource FAILED: MakeResident alloc=0x{:x} hr=0x{:08x}",
                allocation,
                hr as u32
            );
            set_runtime_error(h, hr);
            return;
        }
    };
    let (identity, association) = match assign_outer_allocation(
        &dev.outer,
        allocation,
        descriptor.allocation_generation,
        descriptor.byte_size,
        core::ptr::null_mut(),
        descriptor.has_flag(helios_protocol::HELIOS_HWA2_FLAG_GUEST_PAGE_BACKED),
    ) {
        Ok(assigned) => assigned,
        Err(refusal) => {
            dev.dxvk.discard_associated_texture2d_preflight(preflight);
            drop(resident);
            log_error!(
                "DDI open_resource REFUSED: outer association {:?} alloc=0x{:x} \
                 generation={} bytes={}",
                refusal,
                allocation,
                descriptor.allocation_generation,
                descriptor.byte_size
            );
            set_runtime_error(h, E_OUTOFMEMORY);
            return;
        }
    };
    let opened = OpenedWddmAllocation {
        resident: Some(resident),
        km_resource: a.hKMResource.handle,
        identity,
        association,
    };
    let token = opened.association.outer_allocation_token;
    // The engine must not run its new-texture initializer on an OPENED
    // allocation: the clear lands in the aliased memory and wipes the
    // creator's pixels (2026-09-01: every cross-process shared surface read
    // zero — the missing Start menu). The flag is engine-side only.
    let mut engine_association = opened.association;
    engine_association.association_flags |=
        helios_protocol::HELIOS_RESOURCE_ASSOCIATION_FLAG_OPENED;
    let Some(resource) = dev.dxvk.create_associated_resource(
        2,
        (&open_desc as *const D3D11_TEXTURE2D_DESC) as usize,
        0,
        &engine_association,
        Some(preflight),
    ) else {
        opened.rollback(dev);
        log_error!(
            "DDI open_resource REFUSED: associated image construction failed token={} \
             alloc=0x{:x}",
            token,
            allocation
        );
        set_runtime_error(h, E_OUTOFMEMORY);
        return;
    };

    let raw = resource.as_raw() as usize;
    let (resident, km_resource, outer_allocation) = opened.into_state();
    store_resource(
        h_resource,
        resource,
        resident,
        outer_allocation,
        None,
        km_resource,
        h_rt.handle,
        AllocationOwnership::OpenedByRuntime,
    );
    log_error!(
        "DDI open_resource HRA1 ok: {}x{} array={} mips={} dxgi={} bind=0x{:x} \
         misc=0x{:x} alloc=0x{:x} hKM=0x{:x} token={} lower_bytes={} raw=0x{:x}",
        open_desc.Width,
        open_desc.Height,
        open_desc.ArraySize,
        open_desc.MipLevels,
        open_desc.Format.0,
        open_desc.BindFlags,
        open_desc.MiscFlags,
        allocation,
        km_resource,
        token,
        lower_memory_requirement,
        raw
    );
}

pub(crate) unsafe extern "C" fn calc_size_opened_resource(
    _h: Hdevice,
    _arg: *const ddi::D3D10DDIARG_OPENRESOURCE,
) -> u64 {
    8
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
