//! The surviving Vulkan object operations: create/destroy/bind for images and
//! memory, plus the memory-type choosers that pick a heap for each.
//!
//! Moved verbatim out of `virtio/venus.rs` by T8/R1104.

use super::ring::*;
use super::*;

impl VenusClient {
    /// Allocate HOST_VISIBLE|HOST_COHERENT Venus device memory and bind it to a
    /// HOST3D blob. Returns the memory id (`blob_id`) and virtio resource id.
    /// The ring reply wait guarantees `vkAllocateMemory` has EXECUTED before the
    /// blob create references it (guest-side ordering — the host ctrl queue is
    /// never blocked waiting for this client's allocations).
    pub fn allocate_memory_blob(
        &mut self,
        adapter: &AdapterContext,
        size: u64,
        mappable: bool,
        shareable: bool,
    ) -> Result<HostVisibleBlob, VirtioError> {
        if self.owned_memory_blobs.len() >= MAX_OWNED_MEMORY_BLOBS {
            return Err(VirtioError::OutOfMemory);
        }
        if crate::virtio::KMD_D2_OWNER_ENABLED && !adapter.control_owner().backing_creation_open() {
            return Err(VirtioError::DeviceError);
        }
        let size = round_up_page(size.max(4096));
        let memory_id = self.new_memory_id();
        {
            let w = encode_memory_allocate(
                self.device_id.into(),
                memory_id.into(),
                &MemoryAllocateSpec {
                    pnext: if shareable {
                        MemoryPNext::Export {
                            handle_type: EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF,
                        }
                    } else {
                        MemoryPNext::None
                    },
                    size,
                    memory_type_index: self.memory_type_index.0,
                },
            );
            self.ring_command_expect(
                adapter,
                w.as_slice()?,
                ReplyCheck::new(CMD_ALLOCATE_MEMORY)
                    .mismatch(0x00F6)
                    .refuse_result(0x00F7),
            )?;
        }

        let mut flags = 0;
        if mappable {
            flags |= VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE;
        }
        if shareable {
            flags |= VIRTIO_GPU_BLOB_FLAG_USE_SHAREABLE;
        }
        // The VkDeviceMemory handle IS the virtio blob_id for a KMD-created
        // blob. Under active KMD D2, transfer its finalizer into the canonical
        // resource row before CREATE can become ambiguous. The callback uses
        // this already-held Venus client, so it neither recurses into the mutex
        // nor runs under the owner spinlock.
        let created = if crate::virtio::KMD_D2_OWNER_ENABLED {
            ctrl::resource_create_blob_with_finalizer(
                self.passive(),
                adapter,
                self.ctx_id(),
                VIRTIO_GPU_BLOB_MEM_HOST3D,
                flags,
                memory_id.get(),
                size,
                crate::virtio::control_owner::ResourceBackingFinalizer::memory(memory_id.get()),
                |finalizer| ctrl::finalize_resource_backing_with_client(self, adapter, finalizer),
            )
        } else {
            ctrl::resource_create_blob(
                self.passive(),
                adapter,
                self.ctx_id(),
                VIRTIO_GPU_BLOB_MEM_HOST3D,
                flags,
                memory_id.get(),
                size,
            )
        };
        let res_id = match created {
            Ok(resource_id) => resource_id,
            Err(e) => {
                if !crate::virtio::KMD_D2_OWNER_ENABLED {
                    // Legacy custody never entered OwnerTable, so the caller
                    // still owns this definite-failure cleanup.
                    let _ = self.free_memory_object(adapter, memory_id);
                }
                return Err(e);
            }
        };
        if !crate::virtio::KMD_D2_OWNER_ENABLED {
            let _ = adapter.with_virtio(|v| v.note_blob_size(res_id, size));
        }
        // Capacity was reserved above, so push cannot allocate. Publish the
        // identity only after both Vulkan allocation and resource creation
        // succeeded.
        self.owned_memory_blobs.push(memory_id);
        Ok(HostVisibleBlob {
            blob_id: memory_id.get(),
            res_id,
            size,
        })
    }

    /// Allocate a shareable HOST3D blob from a pure DEVICE_LOCAL memory type.
    ///
    /// This is the sole backing constructor for HVM1 role 4.  It intentionally
    /// omits `USE_MAPPABLE`, refuses BAR/ReBAR memory even when it is also
    /// DEVICE_LOCAL, and has no fallback tier.  The resource is nevertheless
    /// shareable so the exact session Venus context can import the same stock
    /// virtio-gpu resource after its direct WDDM open is admitted.
    pub fn allocate_device_local_memory_blob(
        &mut self,
        adapter: &AdapterContext,
        size: u64,
    ) -> Result<super::DeviceLocalBlob, VirtioError> {
        if self.owned_memory_blobs.len() >= MAX_OWNED_MEMORY_BLOBS {
            return Err(VirtioError::OutOfMemory);
        }
        if crate::virtio::KMD_D2_OWNER_ENABLED && !adapter.control_owner().backing_creation_open() {
            return Err(VirtioError::DeviceError);
        }
        let Some(choice) = helios_kmd_logic::choose_strict_device_local_memory_type(
            &self.memory_type_flags,
            self.memory_type_count,
            u32::MAX,
        ) else {
            return Err(VirtioError::DeviceError);
        };
        let memory_type_index = choice.index();
        let size = round_up_page(size.max(4096));
        let memory_id = self.new_memory_id();
        {
            let w = encode_memory_allocate(
                self.device_id.into(),
                memory_id.into(),
                &MemoryAllocateSpec {
                    pnext: MemoryPNext::Export {
                        handle_type: EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF,
                    },
                    size,
                    memory_type_index,
                },
            );
            self.ring_command_expect(
                adapter,
                w.as_slice()?,
                ReplyCheck::new(CMD_ALLOCATE_MEMORY)
                    .mismatch(0x01F6)
                    .refuse_result(0x01F7),
            )?;
        }

        let created = if crate::virtio::KMD_D2_OWNER_ENABLED {
            ctrl::resource_create_blob_with_finalizer(
                self.passive(),
                adapter,
                self.ctx_id(),
                VIRTIO_GPU_BLOB_MEM_HOST3D,
                VIRTIO_GPU_BLOB_FLAG_USE_SHAREABLE,
                memory_id.get(),
                size,
                crate::virtio::control_owner::ResourceBackingFinalizer::memory(memory_id.get()),
                |finalizer| ctrl::finalize_resource_backing_with_client(self, adapter, finalizer),
            )
        } else {
            ctrl::resource_create_blob(
                self.passive(),
                adapter,
                self.ctx_id(),
                VIRTIO_GPU_BLOB_MEM_HOST3D,
                VIRTIO_GPU_BLOB_FLAG_USE_SHAREABLE,
                memory_id.get(),
                size,
            )
        };
        let res_id = match created {
            Ok(resource_id) => resource_id,
            Err(e) => {
                if !crate::virtio::KMD_D2_OWNER_ENABLED {
                    let _ = self.free_memory_object(adapter, memory_id);
                }
                return Err(e);
            }
        };
        if !crate::virtio::KMD_D2_OWNER_ENABLED {
            let _ = adapter.with_virtio(|v| v.note_blob_size(res_id, size));
        }
        self.owned_memory_blobs.push(memory_id);
        Ok(super::DeviceLocalBlob {
            blob_id: memory_id.get(),
            res_id,
            size,
            memory_type_index,
        })
    }

    /// See [`helios_kmd_logic::choose_host_visible_memory_type`] — the rule is a
    /// pure function of `memory_type_flags`/`memory_type_count`, so it lives
    /// where it can be host-tested.
    pub(super) fn choose_host_visible_memory_type(
        &self,
        memory_type_bits: u32,
    ) -> Option<MemoryTypeChoice> {
        helios_kmd_logic::choose_host_visible_memory_type(
            &self.memory_type_flags,
            self.memory_type_count,
            memory_type_bits,
        )
    }

    /// See [`helios_kmd_logic::choose_device_local_memory_type`].
    pub(super) fn choose_device_local_memory_type(
        &self,
        memory_type_bits: u32,
    ) -> Option<MemoryTypeChoice> {
        helios_kmd_logic::choose_device_local_memory_type(
            &self.memory_type_flags,
            self.memory_type_count,
            memory_type_bits,
        )
    }

    /// Accept a memory-type choice, naming a downgrade in the registry.
    ///
    /// Every selector call site goes through here, so "we asked for
    /// DEVICE_LOCAL and settled for whatever was allowed" can no longer happen
    /// without a breadcrumb. `VnMtDown` records the index that was taken.
    pub(super) fn accept_memory_type(choice: MemoryTypeChoice) -> u32 {
        if let MemoryTypeChoice::Downgraded(index) = choice {
            crate::diag::record_named_bytes(b"VnMtDown", index);
        }
        choice.index()
    }

    pub(super) fn create_linear_scanout_image(
        &mut self,
        adapter: &AdapterContext,
        width: u32,
        height: u32,
    ) -> Result<VkImageId, VirtioError> {
        let image_id = self.new_image_id();
        let w = encode_image_create(
            self.device_id.into(),
            image_id.into(),
            &ImageCreateSpec {
                // VkExternalMemoryImageCreateInfo only. This matches the Linux
                // KMS probe that reached QEMU's egl-headless dmabuf import on
                // NVIDIA.
                external_handle_type: EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF,
                flags: 0,
                format: FORMAT_B8G8R8A8_UNORM,
                width,
                height,
                tiling: IMAGE_TILING_LINEAR,
                usage: IMAGE_USAGE_TRANSFER_SRC | IMAGE_USAGE_TRANSFER_DST,
                initial_layout: IMAGE_LAYOUT_PREINITIALIZED,
            },
        );
        // The raw VkResult of the LINEAR external-DMA_BUF image create is the
        // most likely NVIDIA-venus rejection point for the CachyOS shape, which
        // is why `SdgLImg` carries it verbatim.
        let mut r = self.ring_command_expect(
            adapter,
            w.as_slice()?,
            ReplyCheck::new(CMD_CREATE_IMAGE)
                .mismatch(0x0120)
                .mismatch_marks(b"SdgLImg")
                .refuse_result(0x0121)
                .result_marks(b"SdgLImg"),
        )?;
        if r.read_u64()? == 0 || r.read_u64()? == 0 {
            crate::diag::record_named_bytes(b"SdgLImg", 0xE1);
            diag(0x0122);
            return Err(VirtioError::DeviceError);
        }
        Ok(image_id)
    }

    /// Create the exact KMD-owned GDI OPTIMAL image shape. `ddi_bind_flags` is
    /// the authoritative create-time value; usage is never inferred from
    /// geometry or content.
    pub(super) fn create_optimal_gdi_image(
        &mut self,
        adapter: &AdapterContext,
        width: u32,
        height: u32,
        ddi_bind_flags: u32,
        dxgi_format: u32,
    ) -> Result<VkImageId, VirtioError> {
        if !matches!(dxgi_format, 87 | 88) {
            return Err(VirtioError::DeviceError);
        }
        let vk_format = FORMAT_B8G8R8A8_UNORM;
        // D3D11/DXVK starts every texture with transfer source+destination. The
        // DDI pipeline bits are numerically identical for SRV (0x8) and RTV
        // (0x20); DDI UAV (0x100) is translated to API UAV (0x80), hence the
        // separate test below. PRESENT (0x80) is deliberately not STORAGE.
        const DDI_BIND_SHADER_RESOURCE: u32 = 0x0000_0008;
        const DDI_BIND_RENDER_TARGET: u32 = 0x0000_0020;
        const DDI_BIND_UNORDERED_ACCESS: u32 = 0x0000_0100;
        let mut usage = IMAGE_USAGE_TRANSFER_SRC | IMAGE_USAGE_TRANSFER_DST;
        if ddi_bind_flags & DDI_BIND_SHADER_RESOURCE != 0 {
            usage |= IMAGE_USAGE_SAMPLED;
        }
        if ddi_bind_flags & DDI_BIND_RENDER_TARGET != 0 {
            usage |= IMAGE_USAGE_COLOR_ATTACHMENT;
        }
        if ddi_bind_flags & DDI_BIND_UNORDERED_ACCESS != 0 {
            usage |= IMAGE_USAGE_STORAGE;
        }

        let image_id = self.new_image_id();
        let w = encode_image_create(
            self.device_id.into(),
            image_id.into(),
            &ImageCreateSpec {
                // KMD-created GDI textures use DMA_BUF because the renderer
                // server proxy carries this image across Venus contexts.
                external_handle_type: EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF,
                // Shared color images retain MUTABLE_FORMAT but intentionally
                // have no format-list pNext (DXVK suppresses that list to
                // disable per-image compression metadata which cannot survive
                // cross-device imports).
                flags: IMAGE_CREATE_MUTABLE_FORMAT,
                format: vk_format,
                width,
                height,
                tiling: IMAGE_TILING_OPTIMAL,
                usage,
                initial_layout: IMAGE_LAYOUT_UNDEFINED,
            },
        );
        let mut r = self.ring_command_expect(
            adapter,
            w.as_slice()?,
            ReplyCheck::new(CMD_CREATE_IMAGE)
                .mismatch(0x0130)
                .refuse_result(0x0131)
                .result_marks(b"SdgOImg"),
        )?;
        if r.read_u64()? == 0 || r.read_u64()? == 0 {
            diag(0x0132);
            return Err(VirtioError::DeviceError);
        }
        Ok(image_id)
    }

    pub(super) fn image_memory_requirements(
        &mut self,
        adapter: &AdapterContext,
        image_id: VkImageId,
    ) -> Result<(u64, u32), VirtioError> {
        let mut w = Writer::new();
        w.header(CMD_GET_IMAGE_MEMORY_REQUIREMENTS, CMD_FLAG_GENERATE_REPLY);
        w.handle(self.device_id);
        w.handle(image_id);
        w.count(true);
        // No VkResult in this reply shape: word 1 is the simple-pointer.
        let mut r = self.ring_command_expect(
            adapter,
            w.as_slice()?,
            ReplyCheck::new(CMD_GET_IMAGE_MEMORY_REQUIREMENTS).mismatch(0x0104),
        )?;
        if r.read_u64()? == 0 {
            diag(0x0105);
            return Err(VirtioError::DeviceError);
        }
        let size = r.read_u64()?;
        let _alignment = r.read_u64()?;
        let memory_type_bits = r.read_u32()?;
        Ok((size, memory_type_bits))
    }

    pub(super) fn allocate_optimal_image_memory(
        &mut self,
        adapter: &AdapterContext,
        size: u64,
        memory_type_index: u32,
    ) -> Result<VkDeviceMemoryId, VirtioError> {
        let memory_id = self.new_memory_id();
        let w = encode_memory_allocate(
            self.device_id.into(),
            memory_id.into(),
            &MemoryAllocateSpec {
                // The admitted host reports dedicated allocation as preferred,
                // not required, for the exact OPTIMAL external-image shape.
                // Export-only memory keeps vkAllocateMemory independent of the
                // image object table; vkBindImageMemory supplies the exact
                // image/memory association immediately afterwards.
                pnext: optimal_gdi_memory_pnext(EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF),
                size,
                memory_type_index,
            },
        );
        self.ring_command_expect(
            adapter,
            w.as_slice()?,
            ReplyCheck::new(CMD_ALLOCATE_MEMORY)
                .mismatch(0x0106)
                .mismatch_marks(b"SdgOMem")
                .refuse_result(0x0107)
                .result_marks(b"SdgOMem"),
        )?;
        Ok(memory_id)
    }

    pub(super) fn allocate_export_image_memory(
        &mut self,
        adapter: &AdapterContext,
        size: u64,
        memory_type_index: u32,
    ) -> Result<VkDeviceMemoryId, VirtioError> {
        let memory_id = self.new_memory_id();
        let w = encode_memory_allocate(
            self.device_id.into(),
            memory_id.into(),
            &MemoryAllocateSpec {
                pnext: MemoryPNext::Export {
                    handle_type: EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF,
                },
                size,
                memory_type_index,
            },
        );
        // `SdgLMem` carries the raw VkResult of the exportable-DMA_BUF
        // allocation for the linear scanout image (dedicated-less export
        // alloc).
        self.ring_command_expect(
            adapter,
            w.as_slice()?,
            ReplyCheck::new(CMD_ALLOCATE_MEMORY)
                .mismatch(0x0123)
                .mismatch_marks(b"SdgLMem")
                .refuse_result(0x0124)
                .result_marks(b"SdgLMem"),
        )?;
        Ok(memory_id)
    }

    pub(super) fn bind_image_memory(
        &mut self,
        adapter: &AdapterContext,
        image_id: VkImageId,
        memory_id: VkDeviceMemoryId,
    ) -> Result<(), VirtioError> {
        let mut w = Writer::new();
        w.header(CMD_BIND_IMAGE_MEMORY, CMD_FLAG_GENERATE_REPLY);
        w.handle(self.device_id);
        w.handle(image_id);
        w.handle(memory_id);
        w.u64(0);
        self.ring_command_expect(
            adapter,
            w.as_slice()?,
            ReplyCheck::new(CMD_BIND_IMAGE_MEMORY)
                .mismatch(0x0108)
                .refuse_result(0x0109),
        )?;
        Ok(())
    }

    pub(super) fn image_subresource_layout(
        &mut self,
        adapter: &AdapterContext,
        image_id: VkImageId,
        aspect_mask: u32,
    ) -> Result<(u64, u64), VirtioError> {
        let mut w = Writer::new();
        w.header(CMD_GET_IMAGE_SUBRESOURCE_LAYOUT, CMD_FLAG_GENERATE_REPLY);
        w.handle(self.device_id);
        w.handle(image_id);
        w.count(true);
        w.u32(aspect_mask);
        w.u32(0);
        w.u32(0);
        w.count(true);
        // No VkResult in this reply shape: word 1 is the simple-pointer.
        let mut r = self.ring_command_expect(
            adapter,
            w.as_slice()?,
            ReplyCheck::new(CMD_GET_IMAGE_SUBRESOURCE_LAYOUT).mismatch(0x010A),
        )?;
        if r.read_u64()? == 0 {
            diag(0x010B);
            return Err(VirtioError::DeviceError);
        }
        let offset = r.read_u64()?;
        let _size = r.read_u64()?;
        let row_pitch = r.read_u64()?;
        let _array_pitch = r.read_u64()?;
        let _depth_pitch = r.read_u64()?;
        Ok((offset, row_pitch))
    }
}
