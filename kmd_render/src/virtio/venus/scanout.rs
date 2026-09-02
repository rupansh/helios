//! The KMD-owned LINEAR scan-out fallback: preparing and submitting the copy
//! from the exact Windows primary into a scan-out target, and allocating the
//! OPTIMAL/LINEAR image blobs that back it.
//!
//! Moved verbatim out of `virtio/venus.rs` by T8/R1104.

use super::ring::*;
use super::*;

/// Zero one complete KMD-owned mappable blob through its exact canonical map.
///
/// D2 uses this once when constructing the permanent parking image. The map,
/// byte stores, and unmap all run at PASSIVE with no plane/owner spinlock held;
/// an uncertain unmap is left to OwnerTable's verified-reset custody.
pub(crate) fn zero_host_visible_blob(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    resource_id: u32,
    minimum_size: u64,
) -> Result<(), VirtioError> {
    fill_host_visible_blob(passive, adapter, resource_id, minimum_size, 0)
}

/// What a bounded sample of a mapped blob contains.
pub(crate) struct BlobSample {
    pub nonzero: u32,
    pub max: u8,
}

/// Read up to 64 KiB from the front of an already-prepared blob mapping.
///
/// Bounded on purpose: the caller runs this per present, and the question it
/// answers ("did anything write this at all") does not need the whole surface.
pub(crate) fn sample_host_visible_blob(prep: crate::virtio::gpu::BlobMapPrep) -> Option<BlobSample> {
    const SAMPLE_BYTES: u64 = 64 * 1024;
    let len = prep.size.min(SAMPLE_BYTES);
    if len == 0 {
        return None;
    }
    let map = KernelMap::new(prep.gpa, prep.size, prep.map_cache)?;
    let mut sample = BlobSample { nonzero: 0, max: 0 };
    for i in 0..len {
        let byte = map.read_u8(i);
        if byte != 0 {
            sample.nonzero += 1;
            if byte > sample.max {
                sample.max = byte;
            }
        }
    }
    Some(sample)
}

/// [`zero_host_visible_blob`] with the fill byte named. Nonzero is the
/// `D2ParkPaint` diagnostic only.
pub(crate) fn fill_host_visible_blob(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    resource_id: u32,
    minimum_size: u64,
    value: u8,
) -> Result<(), VirtioError> {
    if !crate::virtio::KMD_D2_OWNER_ENABLED || resource_id == 0 || minimum_size == 0 {
        return Err(VirtioError::DeviceError);
    }
    let prep = ctrl::map_blob_prepare(
        passive,
        adapter,
        crate::virtio::gpu::OwnerFilter::Exactly(None),
        resource_id,
    )?;
    if prep.size < minimum_size {
        let _ = ctrl::resource_unmap_blob(passive, adapter, resource_id);
        return Err(VirtioError::DeviceError);
    }
    let Some(map) = KernelMap::new(prep.gpa, prep.size, prep.map_cache) else {
        let _ = ctrl::resource_unmap_blob(passive, adapter, resource_id);
        return Err(VirtioError::OutOfMemory);
    };
    map.fill(value);
    core::sync::atomic::fence(Ordering::SeqCst);
    drop(map);
    ctrl::resource_unmap_blob(passive, adapter, resource_id)
}

impl VenusClient {
    /// Allocate a KMD-owned GDI texture with the storage contract Windows
    /// requested for `D3DKMDT_GDISURFACE_TEXTURE`: an OPTIMAL BGRA image,
    /// device-local dedicated memory, and DMA_BUF/CROSS_DEVICE export.
    ///
    /// The returned resource is attachable by DWM's renderer-server context.
    /// It is deliberately not mappable and has no row pitch; CPU-visible GDI
    /// surface variants continue to use the separate pitched-buffer path.
    pub fn allocate_optimal_gdi_image_blob(
        &mut self,
        adapter: &AdapterContext,
        width: u32,
        height: u32,
        ddi_bind_flags: u32,
        dxgi_format: u32,
    ) -> Result<OptimalImageBlob, VirtioError> {
        if width == 0 || height == 0 || !matches!(dxgi_format, 87 | 88) {
            return Err(VirtioError::DeviceError);
        }
        if crate::virtio::KMD_D2_OWNER_ENABLED && !adapter.control_owner().backing_creation_open() {
            return Err(VirtioError::DeviceError);
        }

        let image_id =
            self.create_optimal_gdi_image(adapter, width, height, ddi_bind_flags, dxgi_format)?;
        let (required_size, memory_type_bits) =
            match self.image_memory_requirements(adapter, image_id) {
                Ok(requirements) => requirements,
                Err(error) => {
                    let _ = self.destroy_image(adapter, image_id.get());
                    return Err(error);
                }
            };
        let memory_type_index = match self.choose_device_local_memory_type(memory_type_bits) {
            Some(choice) => Self::accept_memory_type(choice),
            None => {
                let _ = self.destroy_image(adapter, image_id.get());
                return Err(VirtioError::DeviceError);
            }
        };
        let allocation_size = round_up_page(required_size.max(4096));
        let memory_id = match self.allocate_optimal_image_memory(
            adapter,
            allocation_size,
            memory_type_index,
        ) {
            Ok(id) => id,
            Err(error) => {
                let _ = self.destroy_image(adapter, image_id.get());
                return Err(error);
            }
        };
        if let Err(error) = self.bind_image_memory(adapter, image_id, memory_id) {
            let _ = self.destroy_image(adapter, image_id.get());
            let _ = self.free_memory_blob(adapter, memory_id.get());
            return Err(error);
        }

        let blob_flags = VIRTIO_GPU_BLOB_FLAG_USE_SHAREABLE | VIRTIO_GPU_BLOB_FLAG_USE_CROSS_DEVICE;
        let created = if crate::virtio::KMD_D2_OWNER_ENABLED {
            ctrl::resource_create_blob_with_finalizer(
                self.passive(),
                adapter,
                self.ctx_id(),
                VIRTIO_GPU_BLOB_MEM_HOST3D,
                blob_flags,
                memory_id.get(),
                allocation_size,
                crate::virtio::control_owner::ResourceBackingFinalizer::image_memory(
                    image_id.get(),
                    memory_id.get(),
                ),
                |finalizer| ctrl::finalize_resource_backing_with_client(self, adapter, finalizer),
            )
        } else {
            ctrl::resource_create_blob(
                self.passive(),
                adapter,
                self.ctx_id(),
                VIRTIO_GPU_BLOB_MEM_HOST3D,
                blob_flags,
                memory_id.get(),
                allocation_size,
            )
        };
        let resource_id = match created {
            Ok(id) => id,
            Err(error) => {
                if !crate::virtio::KMD_D2_OWNER_ENABLED {
                    let _ = self.destroy_image(adapter, image_id.get());
                    let _ = self.free_memory_blob(adapter, memory_id.get());
                }
                return Err(error);
            }
        };
        if !crate::virtio::KMD_D2_OWNER_ENABLED {
            let _ = adapter.with_virtio(|v| v.note_blob_size(resource_id, allocation_size));
        }

        Ok(OptimalImageBlob {
            blob: HostVisibleBlob {
                blob_id: memory_id.get(),
                res_id: resource_id,
                size: allocation_size,
            },
            image_id,
            memory_type_index,
        })
    }

    /// Diagnostic scanout allocation matching the working Linux probe: a plain
    /// LINEAR external DMA_BUF image, host-visible memory, and a HOST3D
    /// MAPPABLE|SHAREABLE blob referencing that memory.
    pub fn allocate_linear_scanout_image_blob(
        &mut self,
        adapter: &AdapterContext,
        width: u32,
        height: u32,
        // D7-3a: a lower bound on the blob so the park matches the desktop
        // primary's byte size — QEMU's OPTIMAL readback rejects a scanout blob
        // smaller than the host GPU's tiled requirement (primary 4587520 vs a
        // tight linear park 4096000 → "OPTIMAL DMA-BUF too small" → black
        // remote view). 0 = the natural (tight) size. The real backing caller
        // passes 0; only the park passes a nonzero floor.
        min_blob_size: u64,
    ) -> Result<ScanoutImageBlob, VirtioError> {
        if crate::virtio::KMD_D2_OWNER_ENABLED && !adapter.control_owner().backing_creation_open() {
            return Err(VirtioError::DeviceError);
        }
        // Stage breadcrumb: `SdgLStg` holds the stage last ENTERED. On an early
        // `?` return it names the exact Venus call that rejected the CachyOS
        // shared-primary shape (mode 16 / real primary), turning the opaque
        // `SdgErr=2` into a precise failing stage. Companion values: `SdgLReq`
        // (mem-req size), `SdgLBit` (memoryTypeBits), `SdgLTyc` (type count),
        // `SdgLImg`/`SdgLMem` (raw VkResults), `SdgLPch`/`SdgLOff` (layout).
        //   1=create image  2=mem-req  3=choose host-visible type
        //   4=alloc export mem  5=bind  6=subresource layout
        //   7=validate pitch/offset  8=create blob  0x10=done
        crate::diag::record_named_bytes(b"SdgLStg", 1);
        let image_id = self.create_linear_scanout_image(adapter, width, height)?;

        crate::diag::record_named_bytes(b"SdgLStg", 2);
        let (req_size, memory_type_bits) = match self.image_memory_requirements(adapter, image_id) {
            Ok(requirements) => requirements,
            Err(error) => {
                let _ = self.destroy_image(adapter, image_id.get());
                return Err(error);
            }
        };
        crate::diag::record_named_bytes(b"SdgLReq", req_size as u32);
        crate::diag::record_named_bytes(b"SdgLBit", memory_type_bits);
        crate::diag::record_named_bytes(b"SdgLTyc", self.memory_type_count);

        crate::diag::record_named_bytes(b"SdgLStg", 3);
        let memory_type_index = match self.choose_host_visible_memory_type(memory_type_bits) {
            Some(choice) => Self::accept_memory_type(choice),
            None => {
                let _ = self.destroy_image(adapter, image_id.get());
                return Err(VirtioError::DeviceError);
            }
        };
        crate::diag::record_named_bytes(b"SdgMt", memory_type_index);
        crate::diag::record_named_bytes(
            b"SdgMf",
            self.memory_type_flags[memory_type_index as usize],
        );
        let alloc_size = round_up_page(req_size.max(4096)).max(round_up_page(min_blob_size));
        if min_blob_size != 0 {
            crate::diag::record_named_bytes(b"SdgParkPad", alloc_size as u32);
        }

        crate::diag::record_named_bytes(b"SdgLStg", 4);
        let memory_id =
            match self.allocate_export_image_memory(adapter, alloc_size, memory_type_index) {
                Ok(memory_id) => memory_id,
                Err(error) => {
                    let _ = self.destroy_image(adapter, image_id.get());
                    return Err(error);
                }
            };

        crate::diag::record_named_bytes(b"SdgLStg", 5);
        if let Err(error) = self.bind_image_memory(adapter, image_id, memory_id) {
            let _ = self.destroy_image(adapter, image_id.get());
            let _ = self.free_memory_blob(adapter, memory_id.get());
            return Err(error);
        }

        crate::diag::record_named_bytes(b"SdgLStg", 6);
        let (offset, row_pitch) =
            match self.image_subresource_layout(adapter, image_id, IMAGE_ASPECT_COLOR) {
                Ok(layout) => layout,
                Err(error) => {
                    let _ = self.destroy_image(adapter, image_id.get());
                    let _ = self.free_memory_blob(adapter, memory_id.get());
                    return Err(error);
                }
            };
        crate::diag::record_named_bytes(b"SdgLPch", row_pitch as u32);
        crate::diag::record_named_bytes(b"SdgLOff", offset as u32);

        crate::diag::record_named_bytes(b"SdgLStg", 7);
        if row_pitch == 0 || row_pitch > u32::MAX as u64 || offset > u32::MAX as u64 {
            diag(0x0125);
            let _ = self.destroy_image(adapter, image_id.get());
            let _ = self.free_memory_blob(adapter, memory_id.get());
            return Err(VirtioError::DeviceError);
        }

        let blob_flags = VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE | VIRTIO_GPU_BLOB_FLAG_USE_SHAREABLE;
        crate::diag::record_named_bytes(b"SdgBFl", blob_flags);
        crate::diag::record_named_bytes(b"SdgLStg", 8);
        let res_id = if crate::virtio::KMD_D2_OWNER_ENABLED {
            ctrl::resource_create_blob_with_finalizer(
                self.passive(),
                adapter,
                self.ctx_id(),
                VIRTIO_GPU_BLOB_MEM_HOST3D,
                blob_flags,
                memory_id.get(),
                alloc_size,
                crate::virtio::control_owner::ResourceBackingFinalizer::image_memory(
                    image_id.get(),
                    memory_id.get(),
                ),
                |finalizer| ctrl::finalize_resource_backing_with_client(self, adapter, finalizer),
            )?
        } else {
            let resource_id = match ctrl::resource_create_blob(
                self.passive(),
                adapter,
                self.ctx_id(),
                VIRTIO_GPU_BLOB_MEM_HOST3D,
                blob_flags,
                memory_id.get(),
                alloc_size,
            ) {
                Ok(resource_id) => resource_id,
                Err(error) => {
                    let _ = self.destroy_image(adapter, image_id.get());
                    let _ = self.free_memory_blob(adapter, memory_id.get());
                    return Err(error);
                }
            };
            let _ = adapter.with_virtio(|v| v.note_blob_size(resource_id, alloc_size));
            resource_id
        };
        crate::diag::record_named_bytes(b"SdgLStg", 0x10);
        Ok(ScanoutImageBlob {
            blob: HostVisibleBlob {
                blob_id: memory_id.get(),
                res_id,
                size: alloc_size,
            },
            image_id,
            memory_type_index,
            row_pitch: row_pitch as u32,
            plane_offset: offset as u32,
        })
    }
}
