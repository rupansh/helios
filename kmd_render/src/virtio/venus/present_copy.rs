//! D5b: the KMD context's one render queue, bound to the GPU-completion ring
//! `PRESENT_COPY_RING_IDX`, its command pool and per-slot command buffers, and
//! the DXVK-identical source alias image the present copy reads through.
//! PASSIVE only, under the adapter venus mutex, like every other client method.

use super::ring::*;
use super::*;
use helios_kmd_logic::present_copy::{
    BUFFER_USAGE_TRANSFER_DST, BUFFER_USAGE_TRANSFER_SRC, CMD_ALLOCATE_COMMAND_BUFFERS,
    CMD_BIND_BUFFER_MEMORY, CMD_CREATE_BUFFER, CMD_CREATE_COMMAND_POOL, CMD_DESTROY_BUFFER,
    CMD_GET_BUFFER_MEMORY_REQUIREMENTS, CMD_GET_DEVICE_QUEUE2, COMMAND_BUFFER_LEVEL_PRIMARY,
    COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER, MAX_SLOTS, PRESENT_COPY_RING_IDX,
    ST_BUFFER_CREATE_INFO, ST_COMMAND_BUFFER_ALLOCATE_INFO, ST_COMMAND_POOL_CREATE_INFO,
    ST_DEVICE_QUEUE_INFO_2, ST_DEVICE_QUEUE_TIMELINE_INFO_MESA,
};

pub(crate) struct PresentCopyObjects {
    queue: VkQueueId,
    /// Kept for the object's lifetime; never freed before the device.
    #[allow(dead_code)]
    pool: VkCommandPoolId,
    command_buffers: [VkCommandBufferId; MAX_SLOTS],
}

/// Raw ids the DISPATCH-side runtime encodes into a copy stream.
#[derive(Clone, Copy)]
pub(crate) struct PresentCopyIds {
    pub ring_idx: u32,
    pub queue: u64,
    pub command_buffers: [u64; MAX_SLOTS],
}

impl VenusClient {
    /// The copy objects, created on first use.
    pub(crate) fn present_copy_ids(
        &mut self,
        adapter: &AdapterContext,
    ) -> Result<PresentCopyIds, VirtioError> {
        if self.present_copy.is_none() {
            let objects = self.create_present_copy_objects(adapter)?;
            self.present_copy = Some(objects);
        }
        let Some(objects) = self.present_copy.as_ref() else {
            return Err(VirtioError::DeviceError);
        };
        let mut command_buffers = [0u64; MAX_SLOTS];
        for (out, id) in command_buffers.iter_mut().zip(objects.command_buffers.iter()) {
            *out = id.get();
        }
        Ok(PresentCopyIds {
            ring_idx: PRESENT_COPY_RING_IDX,
            queue: objects.queue.get(),
            command_buffers,
        })
    }

    fn create_present_copy_objects(
        &mut self,
        adapter: &AdapterContext,
    ) -> Result<PresentCopyObjects, VirtioError> {
        // vkGetDeviceQueue2 with the timeline chain: a queue bound this way
        // retires its fences on GPU completion; the bare vkGetDeviceQueue queue
        // sits on ring 0, whose completion is decode progress only.
        let queue = VkQueueId(self.next_raw());
        {
            let mut w = Writer::new();
            w.header(CMD_GET_DEVICE_QUEUE2, CMD_FLAG_GENERATE_REPLY);
            w.handle(self.device_id);
            w.count(true); // pQueueInfo
            w.i32(ST_DEVICE_QUEUE_INFO_2);
            w.count(true); // pNext -> VkDeviceQueueTimelineInfoMESA
            w.i32(ST_DEVICE_QUEUE_TIMELINE_INFO_MESA);
            w.count(false); // its pNext
            w.u32(PRESENT_COPY_RING_IDX);
            w.u32(0); // flags
            w.u32(0); // queueFamilyIndex
            w.u32(0); // queueIndex
            w.count(true); // pQueue
            w.handle(queue);
            // Reply: [i32 cmd][sp u64][u64 queue] — no VkResult (void).
            let mut r = self.ring_command_expect(
                adapter,
                w.as_slice()?,
                ReplyCheck::new(CMD_GET_DEVICE_QUEUE2)
                    .mismatch(0x0140)
                    .mismatch_marks(b"PcVkQueue"),
            )?;
            if r.read_u64()? == 0 || r.read_u64()? == 0 {
                crate::diag::record_named_bytes(b"PcVkQueue", 0xE1);
                return Err(VirtioError::DeviceError);
            }
        }

        let pool = VkCommandPoolId(self.next_raw());
        {
            let mut w = Writer::new();
            w.header(CMD_CREATE_COMMAND_POOL, CMD_FLAG_GENERATE_REPLY);
            w.handle(self.device_id);
            w.count(true); // pCreateInfo
            w.i32(ST_COMMAND_POOL_CREATE_INFO);
            w.count(false); // pNext
            // RESET_COMMAND_BUFFER: vkBeginCommandBuffer on a reused slot
            // implicitly resets it.
            w.u32(COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER);
            w.u32(0); // queueFamilyIndex
            w.count(false); // pAllocator
            w.count(true); // pCommandPool
            w.handle(pool);
            // Reply: [i32 cmd][i32 VkResult][sp u64][u64 pool]
            let mut r = self.ring_command_expect(
                adapter,
                w.as_slice()?,
                ReplyCheck::new(CMD_CREATE_COMMAND_POOL)
                    .mismatch(0x0141)
                    .refuse_result(0x0142)
                    .result_marks(b"PcVkPool"),
            )?;
            if r.read_u64()? == 0 || r.read_u64()? == 0 {
                crate::diag::record_named_bytes(b"PcVkPool", 0xE1);
                return Err(VirtioError::DeviceError);
            }
        }

        let mut command_buffers = [VkCommandBufferId(self.next_raw()); MAX_SLOTS];
        for slot in command_buffers.iter_mut().skip(1) {
            *slot = VkCommandBufferId(self.next_raw());
        }
        {
            let mut w = Writer::new();
            w.header(CMD_ALLOCATE_COMMAND_BUFFERS, CMD_FLAG_GENERATE_REPLY);
            w.handle(self.device_id);
            w.count(true); // pAllocateInfo
            w.i32(ST_COMMAND_BUFFER_ALLOCATE_INFO);
            w.count(false); // pNext
            w.handle(pool);
            w.i32(COMMAND_BUFFER_LEVEL_PRIMARY);
            w.u32(MAX_SLOTS as u32);
            w.u64(MAX_SLOTS as u64); // pCommandBuffers array_size
            for id in command_buffers {
                w.handle(id);
            }
            // Reply: [i32 cmd][i32 VkResult][array_size u64][u64 × MAX_SLOTS]
            let mut r = self.ring_command_expect(
                adapter,
                w.as_slice()?,
                ReplyCheck::new(CMD_ALLOCATE_COMMAND_BUFFERS)
                    .mismatch(0x0143)
                    .refuse_result(0x0144)
                    .result_marks(b"PcVkCb"),
            )?;
            if r.read_u64()? != MAX_SLOTS as u64 {
                crate::diag::record_named_bytes(b"PcVkCb", 0xE1);
                return Err(VirtioError::DeviceError);
            }
            for expected in command_buffers {
                if r.read_u64()? != expected.get() {
                    crate::diag::record_named_bytes(b"PcVkCb", 0xE2);
                    return Err(VirtioError::DeviceError);
                }
            }
        }

        Ok(PresentCopyObjects {
            queue,
            pool,
            command_buffers,
        })
    }

    /// A second `VkImage` over the memory an ICD image is bound to, created with
    /// the ICD's exact parameters so the host lays it out identically, bound to
    /// the allocation's OWN KMD-device `VkDeviceMemory` (`venus_memory_id`).
    ///
    /// Measured 2026-09-02: this bind works (KMD .449 copied the app's frame),
    /// while every fresh import of the same blob into this device — plain,
    /// dedicated, at the creator's type or the ICD's renderer type — returned
    /// `VK_ERROR_INVALID_EXTERNAL_HANDLE`. The host validation layer notes the
    /// image lists no external handle type while the memory is export-declared,
    /// and that the memory's type is outside the image's `memoryTypeBits`; the
    /// ICD's identical bind trips the same two notes on every frame.
    pub(crate) fn create_present_alias_image(
        &mut self,
        adapter: &AdapterContext,
        spec: &ImageCreateSpec,
        memory_id: u64,
        memory_size: u64,
    ) -> Result<u64, VirtioError> {
        let memory = VkDeviceMemoryId::from_raw(memory_id).ok_or(VirtioError::DeviceError)?;
        let image_id = self.new_image_id();
        let w = encode_image_create(self.device_id.into(), image_id.into(), spec);
        {
            let mut r = self.ring_command_expect(
                adapter,
                w.as_slice()?,
                ReplyCheck::new(CMD_CREATE_IMAGE)
                    .mismatch(0x0145)
                    .refuse_result(0x0146)
                    .result_marks(b"PcVkImg"),
            )?;
            if r.read_u64()? == 0 || r.read_u64()? == 0 {
                crate::diag::record_named_bytes(b"PcVkImg", 0xE1);
                return Err(VirtioError::DeviceError);
            }
        }
        let (required, _memory_type_bits) =
            match self.image_memory_requirements(adapter, image_id) {
                Ok(requirements) => requirements,
                Err(error) => {
                    let _ = self.destroy_image(adapter, image_id.get());
                    return Err(error);
                }
            };
        if required > memory_size {
            crate::diag::record_named_bytes(b"PcAliasSz", required as u32);
            let _ = self.destroy_image(adapter, image_id.get());
            return Err(VirtioError::DeviceError);
        }
        if let Err(error) = self.bind_image_memory(adapter, image_id, memory) {
            let _ = self.destroy_image(adapter, image_id.get());
            return Err(error);
        }
        Ok(image_id.get())
    }

    /// A `VkBuffer` over a pitched standard surface's memory, so the present
    /// copy can write its rows at the authored pitch (`vkCmdCopyImageToBuffer`).
    /// Bound to the allocation's own `VkDeviceMemory`, like the image alias.
    pub(crate) fn create_present_alias_buffer(
        &mut self,
        adapter: &AdapterContext,
        size: u64,
        memory_id: u64,
        memory_size: u64,
    ) -> Result<u64, VirtioError> {
        let memory = VkDeviceMemoryId::from_raw(memory_id).ok_or(VirtioError::DeviceError)?;
        if size == 0 || size > memory_size {
            return Err(VirtioError::DeviceError);
        }
        let buffer = VkBufferId(self.next_raw());
        {
            let mut w = Writer::new();
            w.header(CMD_CREATE_BUFFER, CMD_FLAG_GENERATE_REPLY);
            w.handle(self.device_id);
            w.count(true); // pCreateInfo
            w.i32(ST_BUFFER_CREATE_INFO);
            w.count(false); // pNext
            w.u32(0); // flags
            w.u64(size);
            w.u32(BUFFER_USAGE_TRANSFER_SRC | BUFFER_USAGE_TRANSFER_DST);
            w.u32(helios_kmd_logic::SHARING_MODE_EXCLUSIVE);
            w.u32(0); // queueFamilyIndexCount
            w.u64(0); // pQueueFamilyIndices array_size (EXCLUSIVE)
            w.count(false); // pAllocator
            w.count(true); // pBuffer
            w.handle(buffer);
            // Reply: [i32 cmd][i32 VkResult][sp u64][u64 buffer]
            let mut r = self.ring_command_expect(
                adapter,
                w.as_slice()?,
                ReplyCheck::new(CMD_CREATE_BUFFER)
                    .mismatch(0x0147)
                    .refuse_result(0x0148)
                    .result_marks(b"PcVkBuf"),
            )?;
            if r.read_u64()? == 0 || r.read_u64()? == 0 {
                crate::diag::record_named_bytes(b"PcVkBuf", 0xE1);
                return Err(VirtioError::DeviceError);
            }
        }
        let required = {
            let mut w = Writer::new();
            w.header(CMD_GET_BUFFER_MEMORY_REQUIREMENTS, CMD_FLAG_GENERATE_REPLY);
            w.handle(self.device_id);
            w.handle(buffer);
            w.count(true); // pMemoryRequirements (partial: no payload)
            // Reply (no VkResult): [i32 cmd][sp u64][u64 size][u64 alignment][u32 typeBits]
            let mut r = self.ring_command_expect(
                adapter,
                w.as_slice()?,
                ReplyCheck::new(CMD_GET_BUFFER_MEMORY_REQUIREMENTS).mismatch(0x0149),
            )?;
            if r.read_u64()? == 0 {
                0u64
            } else {
                r.read_u64()?
            }
        };
        if required == 0 || required > memory_size {
            crate::diag::record_named_bytes(b"PcBufReq", required as u32);
            let _ = self.destroy_present_alias_buffer(adapter, buffer.get());
            return Err(VirtioError::DeviceError);
        }
        {
            let mut w = Writer::new();
            w.header(CMD_BIND_BUFFER_MEMORY, CMD_FLAG_GENERATE_REPLY);
            w.handle(self.device_id);
            w.handle(buffer);
            w.handle(memory);
            w.u64(0); // memoryOffset
            // Reply: [i32 cmd][i32 VkResult]
            if let Err(error) = self.ring_command_expect(
                adapter,
                w.as_slice()?,
                ReplyCheck::new(CMD_BIND_BUFFER_MEMORY)
                    .mismatch(0x014A)
                    .refuse_result(0x014B)
                    .result_marks(b"PcVkBind"),
            ) {
                let _ = self.destroy_present_alias_buffer(adapter, buffer.get());
                return Err(error);
            }
        }
        Ok(buffer.get())
    }

    /// Best effort, like [`Self::destroy_image`].
    pub(crate) fn destroy_present_alias_buffer(
        &mut self,
        adapter: &AdapterContext,
        buffer_id: u64,
    ) -> Result<(), VirtioError> {
        let buffer = VkBufferId::from_raw(buffer_id).ok_or(VirtioError::DeviceError)?;
        let mut w = Writer::new();
        w.header(CMD_DESTROY_BUFFER, 0);
        w.handle(self.device_id);
        w.handle(buffer);
        w.count(false); // pAllocator
        self.submit_direct(adapter, w.as_slice()?)
    }
}
