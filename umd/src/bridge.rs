//! cxx bridge to DXVK's C++ engine.
//!
//! The UMD's `d3d10umddi` frontend (Rust) calls into DXVK's `DxvkInstance`/
//! `DxvkAdapter`/`DxvkDevice` through this bridge. The C++ side (`bridge/
//! dxvk_bridge.cpp`) owns the DXVK `Rc<>` objects inside an opaque
//! `HeliosDxvkDevice`; Rust holds it via `UniquePtr`.
//!
//! Backend Vulkan device = the Gate-5a venus ICD; the shim force-selects it via
//! `DXVK_FILTER_DEVICE_NAME="Virtio-GPU Venus"` before creating the instance.

#[cxx::bridge]
mod ffi {
    unsafe extern "C++" {
        include!("dxvk_bridge.h");

        /// Opaque holder for the DXVK instance + adapter + device + the DXVK
        /// D3D11 COM device the DDI forwards to.
        type HeliosDxvkDevice;

        /// Raw `ID3D11Device*` / `ID3D11DeviceContext*` (as usize) the DDI
        /// device-funcs forward to. 0 if not created. Borrowed — the bridge keeps
        /// the owning ref; wrap on the Rust side without taking ownership.
        fn d3d11_device_ptr(self: &HeliosDxvkDevice) -> usize;
        fn d3d11_context_ptr(self: &HeliosDxvkDevice) -> usize;
        unsafe fn create_associated_buffer(
            self: &HeliosDxvkDevice,
            desc_ptr: usize,
            initial_data_ptr: usize,
            package_generation: u64,
            device_generation: u64,
            outer_allocation_token: u64,
            outer_allocation_bytes: u64,
            cpu_mapping: usize,
            association_flags: u32,
        ) -> usize;
        unsafe fn create_associated_texture1d(
            self: &HeliosDxvkDevice,
            desc_ptr: usize,
            initial_data_ptr: usize,
            package_generation: u64,
            device_generation: u64,
            outer_allocation_token: u64,
            outer_allocation_bytes: u64,
            cpu_mapping: usize,
            association_flags: u32,
        ) -> usize;
        unsafe fn create_associated_texture2d(
            self: &HeliosDxvkDevice,
            desc_ptr: usize,
            initial_data_ptr: usize,
            package_generation: u64,
            device_generation: u64,
            outer_allocation_token: u64,
            outer_allocation_bytes: u64,
            cpu_mapping: usize,
            association_flags: u32,
        ) -> usize;
        unsafe fn create_associated_texture3d(
            self: &HeliosDxvkDevice,
            desc_ptr: usize,
            initial_data_ptr: usize,
            package_generation: u64,
            device_generation: u64,
            outer_allocation_token: u64,
            outer_allocation_bytes: u64,
            cpu_mapping: usize,
            association_flags: u32,
        ) -> usize;
        fn feed_trace_timestamp_ns(self: &HeliosDxvkDevice) -> u64;
        fn feed_trace_present_callback(self: &HeliosDxvkDevice, duration_ns: u64);
        /// # Safety
        /// `deferred_context_ptr` is borrowed and live. `command_list_ptr`
        /// transfers one owned COM reference on true; false leaves ownership
        /// with the caller, which must reconstruct and release it.
        unsafe fn recycle_deferred_command_list(
            self: &HeliosDxvkDevice,
            deferred_context_ptr: usize,
            command_list_ptr: usize,
        ) -> bool;
        /// # Safety
        /// `deferred_context_ptr` must be a live deferred context created by
        /// this bridge immediately before this call.
        unsafe fn enable_deferred_context_ddi_logical_reset(
            self: &HeliosDxvkDevice,
            deferred_context_ptr: usize,
        ) -> bool;
        unsafe fn create_vertex_shader(
            self: &HeliosDxvkDevice,
            code: *const u8,
            len: usize,
        ) -> usize;
        unsafe fn create_pixel_shader(
            self: &HeliosDxvkDevice,
            code: *const u8,
            len: usize,
        ) -> usize;
        /// >=11.1 DDI shader create carrying the typed I/O signatures. `kind`:
        /// 0 = vertex, 1 = pixel, 2 = geometry. `sig_words` layout:
        /// [n_in, n_out, (sysval, register, mask, comptype, stream) x n_in,
        /// the same x n_out].
        unsafe fn create_shader_sig(
            self: &HeliosDxvkDevice,
            kind: u32,
            code: *const u8,
            len: usize,
            sig_words: *const u32,
            sig_words_len: usize,
        ) -> usize;
        /// Tessellation shader create carrying input/output/patch-constant
        /// signatures. `kind`: 0 = hull, 1 = domain. `sig_words` layout:
        /// [n_in, n_out, n_patch, then (sysval, register, mask, comptype,
        /// stream) entries for each group].
        unsafe fn create_tess_shader_sig(
            self: &HeliosDxvkDevice,
            kind: u32,
            code: *const u8,
            len: usize,
            sig_words: *const u32,
            sig_words_len: usize,
        ) -> usize;
        /// Flip-model identity rotation: texture i takes texture i+1's DXVK
        /// storage (memory + VkImage + KMT handles); the last takes the
        /// first's. The swap executes on the CS thread (ordered); no drain.
        unsafe fn rotate_resource_backings(
            self: &HeliosDxvkDevice,
            d3d11_resource_ptrs: *const usize,
            count: usize,
        ) -> bool;
        /// Flush DXVK's CS thread and wait only through the actual
        /// vkQueueSubmit edge. This never waits for GPU completion.
        fn flush_submitted(self: &HeliosDxvkDevice) -> bool;
        unsafe fn create_geometry_shader(
            self: &HeliosDxvkDevice,
            code: *const u8,
            len: usize,
        ) -> usize;
        unsafe fn create_hull_shader(self: &HeliosDxvkDevice, code: *const u8, len: usize)
            -> usize;
        unsafe fn create_domain_shader(
            self: &HeliosDxvkDevice,
            code: *const u8,
            len: usize,
        ) -> usize;
        unsafe fn create_compute_shader(
            self: &HeliosDxvkDevice,
            code: *const u8,
            len: usize,
        ) -> usize;

        /// Create a DXVK instance and logical device on the Helios venus adapter.
        ///
        /// The instance, GIPA and module are the exact A5-owned construction
        /// edge. The bridge never asks a Vulkan loader for another instance.
        /// The nonzero LUID must match one physical device exactly.
        fn helios_dxvk_create_device(
            vk_instance: usize,
            get_instance_proc_addr: usize,
            icd_module_base: usize,
            luid_low: u32,
            luid_high: i32,
            outer_context: usize,
            outer_begin: usize,
            outer_finish: usize,
            outer_join: usize,
            outer_allocate: usize,
            outer_teardown_begin: usize,
            outer_retire: usize,
        ) -> UniquePtr<HeliosDxvkDevice>;
    }
}

// ---------------------------------------------------------------------------
// Safe wrappers: one owned/borrowed decision per bridge entry point.
// ---------------------------------------------------------------------------
//
// Thirteen bridge methods return a COM pointer as a bare `usize`. Two are
// BORROWED -- the bridge keeps the owning reference -- and eleven are OWNED and
// the Rust side must `Release`. Before R813 that discipline was a doc comment on
// one side and a `// SAFETY:` comment on the other, repeated at every call site.
// Every existing site was correct; the exposure was entirely future sites, and
// both failure modes are silent:
//
//   * adopting a borrowed pointer  -> a double release. `ID3D11Resource::
//     from_raw(dev.dxvk.d3d11_device_ptr() as *mut c_void)` is type-correct,
//     compiles, and drops the device's only reference at end of scope --
//     destroying the D3D11 device under a running DDI.
//   * wrapping an owned pointer in `ManuallyDrop` -> a leak.
//
// Each surfaces as a much later crash in dwm. The wrappers below make the
// correct adoption exist in exactly ONE place per entry point; R815 is what
// makes the wrong one unreachable.

use core::ffi::c_void;
use core::mem::ManuallyDrop;

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Buffer, ID3D11Device, ID3D11DeviceContext, ID3D11Resource, ID3D11Texture1D,
    ID3D11Texture2D, ID3D11Texture3D,
};

impl ffi::HeliosDxvkDevice {
    // -- borrowed ----------------------------------------------------------
    //
    // `ManuallyDrop` is the whole point: the bridge owns the reference, so the
    // returned wrapper must never release it.

    pub(crate) fn d3d11_device(&self) -> Option<ManuallyDrop<ID3D11Device>> {
        let p = self.d3d11_device_ptr();
        // SAFETY: a non-zero `d3d11_device_ptr` is the bridge's live
        // ID3D11Device, kept alive by the bridge for as long as this device
        // exists. ManuallyDrop borrows it without taking a reference.
        (p != 0).then(|| ManuallyDrop::new(unsafe { ID3D11Device::from_raw(p as *mut c_void) }))
    }

    pub(crate) fn d3d11_context(&self) -> Option<ManuallyDrop<ID3D11DeviceContext>> {
        let p = self.d3d11_context_ptr();
        // SAFETY: as above, for the immediate context.
        (p != 0)
            .then(|| ManuallyDrop::new(unsafe { ID3D11DeviceContext::from_raw(p as *mut c_void) }))
    }
}
// NOT wrapped, deliberately: the eight shader creates.
//
// R813 suggests an `Option<usize>` (or NonZero) wrapper for them too. Audited
// instead: all TEN shader-create call sites already guard the bridge's
// 0-failure sentinel with `if raw != 0` before `store_raw_com`, and storing a
// zero would be harmless anyway -- `load_com` null-checks the slot. There is no
// wrong-adoption hazard here either, because the result goes into a slot as a
// raw word rather than being wrapped as owned or borrowed. A newtype would be
// ceremony across ten correct sites, so it is left out by the review's own
// "rejected as cosmetic" standard.

// ---------------------------------------------------------------------------
// BridgeDevice: the sealed public API (R815)
// ---------------------------------------------------------------------------
//
// After R813 the safe wrappers exist, but the raw `usize`-returning methods
// remain callable at every site, so nothing prevents a future caller choosing
// the wrong adoption. Module privacy alone is NOT sufficient: cxx generates the
// raw methods as INHERENT methods on the public opaque type, and inherent
// methods of a re-exported public type stay callable regardless of module
// visibility. A newtype with no `Deref` is the only encoding that actually
// seals them -- which is why `BridgeDevice` deliberately has none, and why
// `inner` is private.
//
// The C++ side still returns `usize`, so the ABI is unchanged and this
// migration cannot break the wire.

/// The DXVK bridge device, with the raw cxx surface sealed off.
pub struct BridgeDevice {
    inner: cxx::UniquePtr<ffi::HeliosDxvkDevice>,
}

impl BridgeDevice {
    /// Create a DXVK instance and logical device on the Helios venus adapter.
    /// `None` when the bridge returned a null device (no adapter, creation
    /// threw, ...) -- folding the old `is_null()` check into construction so a
    /// `BridgeDevice` that exists is always usable.
    pub fn create(
        vk_instance: usize,
        get_instance_proc_addr: usize,
        icd_module_base: usize,
        luid_low: u32,
        luid_high: i32,
        outer_context: usize,
        outer_begin: usize,
        outer_finish: usize,
        outer_join: usize,
        outer_allocate: usize,
        outer_teardown_begin: usize,
        outer_retire: usize,
    ) -> Option<Self> {
        let inner = ffi::helios_dxvk_create_device(
            vk_instance,
            get_instance_proc_addr,
            icd_module_base,
            luid_low,
            luid_high,
            outer_context,
            outer_begin,
            outer_finish,
            outer_join,
            outer_allocate,
            outer_teardown_begin,
            outer_retire,
        );
        (!inner.is_null()).then_some(Self { inner })
    }

    /// The only path from the newtype to the sealed type, and it is private.
    fn get(&self) -> Option<&ffi::HeliosDxvkDevice> {
        self.inner.as_ref()
    }

    /// Drop the complete DXVK/D3D11 engine while the boxed A5 outer context is
    /// still attached. Its destructor may flush/wait and therefore must run
    /// before HQC1, HQA1, or the sole VkInstance are torn down.
    pub(crate) fn shutdown(&mut self) {
        let inner = core::mem::replace(&mut self.inner, cxx::UniquePtr::null());
        drop(inner);
    }

    // -- borrowed COM ------------------------------------------------------

    pub(crate) fn d3d11_device(&self) -> Option<ManuallyDrop<ID3D11Device>> {
        self.get()?.d3d11_device()
    }

    pub(crate) fn d3d11_context(&self) -> Option<ManuallyDrop<ID3D11DeviceContext>> {
        self.get()?.d3d11_context()
    }

    /// Create one resource through the immutable HRA1 construction edge.
    /// `kind` is the closed D3D11 resource dimension set: 0 buffer, 1 texture
    /// 1D, 2 texture 2D, 3 texture 3D. The returned pointer is the owning
    /// interface for that exact kind and is converted to ID3D11Resource here.
    pub(crate) unsafe fn create_associated_resource(
        &self,
        kind: u32,
        desc_ptr: usize,
        initial_data_ptr: usize,
        association: &helios_protocol::HeliosResourceAssociationV1,
    ) -> Option<ID3D11Resource> {
        let d = self.get()?;
        let raw = match kind {
            0 => unsafe {
                d.create_associated_buffer(
                    desc_ptr,
                    initial_data_ptr,
                    association.package_generation,
                    association.device_generation,
                    association.outer_allocation_token,
                    association.outer_allocation_bytes,
                    association.cpu_mapping as usize,
                    association.association_flags,
                )
            },
            1 => unsafe {
                d.create_associated_texture1d(
                    desc_ptr,
                    initial_data_ptr,
                    association.package_generation,
                    association.device_generation,
                    association.outer_allocation_token,
                    association.outer_allocation_bytes,
                    association.cpu_mapping as usize,
                    association.association_flags,
                )
            },
            2 => unsafe {
                d.create_associated_texture2d(
                    desc_ptr,
                    initial_data_ptr,
                    association.package_generation,
                    association.device_generation,
                    association.outer_allocation_token,
                    association.outer_allocation_bytes,
                    association.cpu_mapping as usize,
                    association.association_flags,
                )
            },
            3 => unsafe {
                d.create_associated_texture3d(
                    desc_ptr,
                    initial_data_ptr,
                    association.package_generation,
                    association.device_generation,
                    association.outer_allocation_token,
                    association.outer_allocation_bytes,
                    association.cpu_mapping as usize,
                    association.association_flags,
                )
            },
            _ => 0,
        };
        if raw == 0 {
            return None;
        }
        match kind {
            0 => Some(
                ID3D11Buffer::from_raw(raw as *mut core::ffi::c_void)
                    .cast::<ID3D11Resource>()
                    .ok()?,
            ),
            1 => Some(
                ID3D11Texture1D::from_raw(raw as *mut core::ffi::c_void)
                    .cast::<ID3D11Resource>()
                    .ok()?,
            ),
            2 => Some(
                ID3D11Texture2D::from_raw(raw as *mut core::ffi::c_void)
                    .cast::<ID3D11Resource>()
                    .ok()?,
            ),
            3 => Some(
                ID3D11Texture3D::from_raw(raw as *mut core::ffi::c_void)
                    .cast::<ID3D11Resource>()
                    .ok()?,
            ),
            _ => None,
        }
    }

    /// # Safety
    /// `deferred_context_ptr` must be a live DXVK deferred interface.
    /// `command_list_ptr` transfers one owned COM reference iff this returns
    /// true; false leaves it owned by the caller.
    pub(crate) unsafe fn recycle_deferred_command_list(
        &self,
        deferred_context_ptr: usize,
        command_list_ptr: usize,
    ) -> bool {
        self.get().is_some_and(|d| unsafe {
            d.recycle_deferred_command_list(deferred_context_ptr, command_list_ptr)
        })
    }

    /// # Safety
    /// `deferred_context_ptr` must be the newly-created live DXVK deferred
    /// context owned by this UMD device.
    pub(crate) unsafe fn enable_deferred_context_ddi_logical_reset(
        &self,
        deferred_context_ptr: usize,
    ) -> bool {
        self.get().is_some_and(|d| unsafe {
            d.enable_deferred_context_ddi_logical_reset(deferred_context_ptr)
        })
    }

    // -- scalar passthroughs ----------------------------------------------

    pub(crate) fn feed_trace_timestamp_ns(&self) -> u64 {
        self.get().map_or(0, |d| d.feed_trace_timestamp_ns())
    }

    pub(crate) fn feed_trace_present_callback(&self, duration_ns: u64) {
        if let Some(d) = self.get() {
            d.feed_trace_present_callback(duration_ns);
        }
    }

    pub(crate) fn flush_submitted(&self) -> bool {
        self.get().is_some_and(|d| d.flush_submitted())
    }

    // -- pointer-laundering passthroughs -----------------------------------
    //
    // `unsafe` for the reason R814 established: each hands the bridge a raw
    // address it reinterpret_casts.

    /// # Safety
    /// `d3d11_resource_ptrs` must point at `count` live `ID3D11Resource*`.
    pub(crate) unsafe fn rotate_resource_backings(
        &self,
        d3d11_resource_ptrs: *const usize,
        count: usize,
    ) -> bool {
        self.get()
            .is_some_and(|d| unsafe { d.rotate_resource_backings(d3d11_resource_ptrs, count) })
    }

    // -- shader creates ----------------------------------------------------
    //
    // These return the bridge's owned COM pointer as a raw word because the IA
    // caches key on the pointer VALUE; see the note above on why they are not
    // additionally newtyped.

    /// # Safety
    /// `code` must point at `len` readable bytes.
    pub(crate) unsafe fn create_vertex_shader(&self, code: *const u8, len: usize) -> usize {
        self.get()
            .map_or(0, |d| unsafe { d.create_vertex_shader(code, len) })
    }

    /// # Safety
    /// `code` must point at `len` readable bytes.
    pub(crate) unsafe fn create_pixel_shader(&self, code: *const u8, len: usize) -> usize {
        self.get()
            .map_or(0, |d| unsafe { d.create_pixel_shader(code, len) })
    }

    /// # Safety
    /// `code` must point at `len` readable bytes.
    pub(crate) unsafe fn create_geometry_shader(&self, code: *const u8, len: usize) -> usize {
        self.get()
            .map_or(0, |d| unsafe { d.create_geometry_shader(code, len) })
    }

    /// # Safety
    /// `code` must point at `len` readable bytes.
    pub(crate) unsafe fn create_hull_shader(&self, code: *const u8, len: usize) -> usize {
        self.get()
            .map_or(0, |d| unsafe { d.create_hull_shader(code, len) })
    }

    /// # Safety
    /// `code` must point at `len` readable bytes.
    pub(crate) unsafe fn create_domain_shader(&self, code: *const u8, len: usize) -> usize {
        self.get()
            .map_or(0, |d| unsafe { d.create_domain_shader(code, len) })
    }

    /// # Safety
    /// `code` must point at `len` readable bytes.
    pub(crate) unsafe fn create_compute_shader(&self, code: *const u8, len: usize) -> usize {
        self.get()
            .map_or(0, |d| unsafe { d.create_compute_shader(code, len) })
    }

    /// # Safety
    /// `code`/`sig_words` must point at `len`/`sig_words_len` readable items.
    pub(crate) unsafe fn create_shader_sig(
        &self,
        kind: u32,
        code: *const u8,
        len: usize,
        sig_words: *const u32,
        sig_words_len: usize,
    ) -> usize {
        self.get().map_or(0, |d| unsafe {
            d.create_shader_sig(kind, code, len, sig_words, sig_words_len)
        })
    }

    /// # Safety
    /// `code`/`sig_words` must point at `len`/`sig_words_len` readable items.
    pub(crate) unsafe fn create_tess_shader_sig(
        &self,
        kind: u32,
        code: *const u8,
        len: usize,
        sig_words: *const u32,
        sig_words_len: usize,
    ) -> usize {
        self.get().map_or(0, |d| unsafe {
            d.create_tess_shader_sig(kind, code, len, sig_words, sig_words_len)
        })
    }
}
