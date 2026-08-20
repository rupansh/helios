//! Narrow C++ bridge to the statically linked, record-only vkd3d engine.
//!
//! Mesa A5 supplies the exact `VkInstance`, direct GIPA, and package module
//! provenance. This module never loads or enumerates a Vulkan module and does
//! not expose a resource registry or an independent queue timeline.

#[cxx::bridge]
mod ffi {
    unsafe extern "C++" {
        include!("vkd3d_bridge.h");

        type HeliosVkd3dDevice;

        fn d3d12_device_ptr(self: &HeliosVkd3dDevice) -> usize;
        unsafe fn create_associated_heap(
            self: &HeliosVkd3dDevice,
            desc_ptr: usize,
            package_generation: u64,
            device_generation: u64,
            outer_allocation_token: u64,
            outer_allocation_bytes: u64,
            cpu_mapping: usize,
            association_flags: u32,
        ) -> usize;
        unsafe fn create_associated_queue(
            self: &HeliosVkd3dDevice,
            desc_ptr: usize,
            callback_context: usize,
            begin_address: usize,
            finish_address: usize,
            join_address: usize,
            out_vk_family_index: *mut u32,
            out_vk_queue_index: *mut u32,
        ) -> usize;

        fn helios_vkd3d_bridge_create_device(
            vk_instance: usize,
            get_instance_proc_addr: usize,
            icd_module_base: usize,
            luid_low: u32,
            luid_high: i32,
            outer_context: usize,
            outer_allocation_create: usize,
            outer_allocation_teardown_begin: usize,
            outer_allocation_begin: usize,
            outer_allocation_finish: usize,
            outer_allocation_retire: usize,
        ) -> UniquePtr<HeliosVkd3dDevice>;

        unsafe fn helios_vkd3d_bridge_serialize_root_signature(
            desc: usize,
            version: u32,
            blob_out: *mut usize,
            err_out: *mut usize,
        ) -> i32;
    }
}

use core::ffi::c_void;
use core::mem::ManuallyDrop;

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D12::{ID3D12CommandQueue, ID3D12Device, ID3D12Heap};

pub struct BridgeDevice12 {
    inner: cxx::UniquePtr<ffi::HeliosVkd3dDevice>,
}

impl BridgeDevice12 {
    pub fn create(
        vk_instance: usize,
        get_instance_proc_addr: usize,
        icd_module_base: usize,
        luid_low: u32,
        luid_high: i32,
        outer_context: usize,
        outer_allocation_create: usize,
        outer_allocation_teardown_begin: usize,
        outer_allocation_begin: usize,
        outer_allocation_finish: usize,
        outer_allocation_retire: usize,
    ) -> Option<Self> {
        let inner = ffi::helios_vkd3d_bridge_create_device(
            vk_instance,
            get_instance_proc_addr,
            icd_module_base,
            luid_low,
            luid_high,
            outer_context,
            outer_allocation_create,
            outer_allocation_teardown_begin,
            outer_allocation_begin,
            outer_allocation_finish,
            outer_allocation_retire,
        );
        (!inner.is_null()).then_some(Self { inner })
    }

    fn get(&self) -> Option<&ffi::HeliosVkd3dDevice> {
        self.inner.as_ref()
    }

    pub(crate) fn d3d12_device(&self) -> Option<ManuallyDrop<ID3D12Device>> {
        let raw = self.get()?.d3d12_device_ptr();
        (raw != 0).then(|| {
            // SAFETY: the bridge retains the owning reference; ManuallyDrop
            // prevents this borrowed wrapper from releasing it.
            ManuallyDrop::new(unsafe { ID3D12Device::from_raw(raw as *mut c_void) })
        })
    }

    pub(crate) unsafe fn create_associated_heap(
        &self,
        desc_ptr: usize,
        association: &helios_protocol::HeliosResourceAssociationV1,
    ) -> Option<ID3D12Heap> {
        let raw = unsafe {
            self.get()?.create_associated_heap(
                desc_ptr,
                association.package_generation,
                association.device_generation,
                association.outer_allocation_token,
                association.outer_allocation_bytes,
                association.cpu_mapping as usize,
                association.association_flags,
            )
        };
        (raw != 0).then(|| unsafe { ID3D12Heap::from_raw(raw as *mut c_void) })
    }

    pub(crate) unsafe fn create_associated_queue(
        &self,
        desc_ptr: usize,
        callback_context: usize,
        begin_address: usize,
        finish_address: usize,
        join_address: usize,
    ) -> Option<(ID3D12CommandQueue, u32, u32)> {
        let mut family = u32::MAX;
        let mut index = u32::MAX;
        let raw = unsafe {
            self.get()?.create_associated_queue(
                desc_ptr,
                callback_context,
                begin_address,
                finish_address,
                join_address,
                &mut family,
                &mut index,
            )
        };
        (raw != 0 && family != u32::MAX && index != u32::MAX).then(|| {
            // SAFETY: successful construction transfers one owning reference.
            let queue = unsafe { ID3D12CommandQueue::from_raw(raw as *mut c_void) };
            (queue, family, index)
        })
    }
}

/// Forward root-signature serialization to the package engine.
///
/// # Safety
/// `desc` and both non-null output pointers must remain valid for this call.
pub(crate) unsafe fn serialize_root_signature(
    desc: usize,
    version: u32,
    blob_out: *mut usize,
    err_out: *mut usize,
) -> i32 {
    unsafe { ffi::helios_vkd3d_bridge_serialize_root_signature(desc, version, blob_out, err_out) }
}
