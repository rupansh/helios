//! Minimal in-kernel venus (Vulkan-over-virtio-gpu) client.
//!
//! WHY THIS EXISTS. VidMm drops a system-RAM-backed memory segment, but the WDDM
//! decorative page-table segment must be **device-BAR memory backed by real host
//! memory**. venus host-visible memory, mapped into the host-visible BAR window,
//! is exactly that: the host GPU owns the real allocation, the guest sees it
//! through the SHARED_MEMORY_CFG/HOST_VISIBLE BAR at `host_visible.base + offset`,
//! and it is CPU-coherent. This module self-allocates ONE 16-MiB
//! HOST_VISIBLE|HOST_COHERENT `VkDeviceMemory` over venus at device-init time and
//! returns its guest-physical window address so `query_segments` can report it as
//! the VidMm page-table segment.
//!
//! HOW IT WORKS. venus is the Mesa Vulkan-passthrough protocol: an opaque
//! command stream the host (virglrenderer's venus decoder) executes. We bootstrap
//! the venus command *ring* (a shared-memory FIFO described to the host by
//! `vkCreateRingMESA`, sent directly via `VIRTIO_GPU_CMD_SUBMIT_3D`), then drive
//! the normal Vulkan bring-up — instance, physical device, memory properties,
//! device, allocate-memory — through that ring, reading replies from a second
//! shared-memory blob. All wire encodings are byte-for-byte verified against
//! `icd/mesa/src/virtio/venus-protocol/vn_protocol_driver_*.h` and `vn_ring.c`.
//!
//! IRQL. The entire flow runs at PASSIVE_LEVEL — bring-up from
//! `DxgkDdiStartDevice`, runtime allocation from `DxgkDdiCreateAllocation` /
//! `DxgkDdiDestroyAllocation` under the adapter's PASSIVE venus mutex
//! (`AdapterContext::with_venus_client`). Control-queue submissions ride
//! `virtio::ctrl` (PASSIVE KEVENT waits under the hood); the venus ring-head
//! waits are PASSIVE sleep-polls (short spin burst, then 1 ms sleeps) — the vn
//! ring has no interrupt to wait on (the host writes progress into the ring
//! shmem only), and the old DISPATCH spins under the device spinlock were one
//! of the 2026-07-04 convoy/poison classes. NOTHING here ever holds the virtio
//! spinlock across a wait.

use alloc::vec::Vec;
use core::num::NonZeroU64;
use core::sync::atomic::{compiler_fence, fence, Ordering};

use helios_protocol::{
    VIRTIO_GPU_BLOB_FLAG_USE_CROSS_DEVICE, VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE,
    VIRTIO_GPU_BLOB_FLAG_USE_SHAREABLE, VIRTIO_GPU_BLOB_MEM_HOST3D, VIRTIO_GPU_MAP_CACHE_CACHED,
    VIRTIO_GPU_MAP_CACHE_UNCACHED, VIRTIO_GPU_MAP_CACHE_WC,
};
use wdk_sys::ntddk::{MmMapIoSpace, MmUnmapIoSpace};
use wdk_sys::{_MEMORY_CACHING_TYPE, PHYSICAL_ADDRESS};

use super::ctrl;
use super::VirtioError;
use crate::adapter::AdapterContext;
use crate::irql::PassiveLevel;

mod bringup;
mod commands;
mod present_copy;
mod protocol;
mod ring;
mod scanout;

pub(crate) use bringup::*;
pub(crate) use protocol::*;
use ring::*;
pub(crate) use scanout::{fill_host_visible_blob, sample_host_visible_blob, zero_host_visible_blob};

/// Declare one handle newtype per Vulkan object class.
///
/// One untyped counter mints every image, memory, and device handle in the same
/// host object space. Distinct newtypes keep a swapped image/memory argument
/// from compiling.
///
/// `NonZeroU64` also retires the "0 means absent" convention and the ~30
/// scattered `if x != 0` guards that implemented it: `Option<VkImageId>` is the
/// same size as the `u64` it replaces, and "not yet valid" stops being encodable
/// as a legal-looking handle the encoder will happily write into the stream.
///
/// The macro exists so the identical definitions cannot drift; it expands to
/// exactly the three items written below and nothing else.
macro_rules! vk_handle {
    ($(#[$attr:meta])* $name:ident) => {
        $(#[$attr])*
        #[derive(Clone, Copy, PartialEq, Eq)]
        pub struct $name(NonZeroU64);

        impl $name {
            /// The raw handle, for the wire encoder and for the `AtomicU64`
            /// mirrors in `ddi/create_allocation.rs`.
            pub fn get(self) -> u64 {
                self.0.get()
            }

            /// Rebuild from a raw handle — a mirror read, or a value decoded
            /// from a host reply. `0` is not a handle.
            pub fn from_raw(raw: u64) -> Option<Self> {
                NonZeroU64::new(raw).map(Self)
            }
        }

        impl From<$name> for u64 {
            fn from(id: $name) -> u64 {
                id.get()
            }
        }
    };
}

vk_handle!(
    /// `VkImage`.
    VkImageId
);
vk_handle!(
    /// `VkDeviceMemory`. Doubles as the virtio-gpu `blob_id` for KMD-created
    /// blobs — that coupling is real and deliberate, so the raw value still
    /// crosses into `ctrl::resource_create_blob` as a `u64`.
    VkDeviceMemoryId
);
vk_handle!(
    /// `VkDevice`.
    VkDeviceId
);
vk_handle!(
    /// `VkQueue` — the KMD context's one render queue (D5b present copy).
    VkQueueId
);
vk_handle!(
    /// `VkCommandPool` for the present-copy command buffers.
    VkCommandPoolId
);
vk_handle!(
    /// `VkCommandBuffer`, one per present-copy slot.
    VkCommandBufferId
);
vk_handle!(
    /// `VkBuffer` — the present copy's alias over a pitched standard surface.
    VkBufferId
);

/// The result of [`allocate_host_visible_blob`]: a venus-backed, BAR-visible,
/// CPU-coherent region for VidMm's page-table segment.
#[derive(Clone, Copy)]
pub struct HostVisibleBlob {
    /// The venus `VkDeviceMemory` id, which is also the virtio-gpu `blob_id`.
    pub blob_id: u64,
    /// The virtio-gpu resource id of the mapped blob (for teardown / unref).
    pub res_id: u32,
    /// Page-rounded size mapped into the window.
    pub size: u64,
}

/// A HOST3D blob backed by a pure DEVICE_LOCAL, non-HOST_VISIBLE Vulkan
/// allocation. Unlike [`HostVisibleBlob`], this object is deliberately not
/// mappable.
pub struct DeviceLocalBlob {
    pub blob_id: u64,
    pub res_id: u32,
    pub size: u64,
    pub memory_type_index: u32,
}

pub struct ScanoutImageBlob {
    pub blob: HostVisibleBlob,
    pub image_id: VkImageId,
    pub memory_type_index: u32,
    pub row_pitch: u32,
    pub plane_offset: u32,
}

/// KMD-owned OPTIMAL image exported as a cross-context DMA_BUF resource.
///
/// Unlike [`ScanoutImageBlob`], this image has no linear row layout.  Its
/// Vulkan image create-info and external-memory transport are the complete
/// storage contract; callers must never reinterpret the backing as a buffer.
pub struct OptimalImageBlob {
    pub blob: HostVisibleBlob,
    pub image_id: VkImageId,
    pub memory_type_index: u32,
}

/// Every owned memory blob is also tracked by VirtioGpu's bounded blob table,
/// so the transport capacity is the exact upper bound.
const MAX_OWNED_MEMORY_BLOBS: usize = crate::virtio::gpu::MAX_BLOBS;

/// Bring-up stage 2: an instance and a physical device exist on the ring.
///
/// Exists only between `VenusRing::into_instance` and `into_device`. Its only
/// job is to make "we have an instance but not a device yet" a state you cannot
/// call `allocate_memory_blob` from.
struct VenusInstance {
    ring: VenusRing,
    phys_dev_id: NonZeroU64,
}

/// A `memoryTypeIndex` proven to be below the host's reported `memoryTypeCount`.
///
/// The bring-up used to leave this field 0 until stage 6, which is a *legal*
/// index — so "not yet chosen" and "chose type 0" were the same value.
#[derive(Clone, Copy)]
struct MemoryTypeIndex(u32);

/// The persistent venus client owned by the adapter for the device lifetime.
///
/// Holds the ring/reply BAR mappings and the live Vulkan object ids. Dropping it
/// unmaps the kernel mappings; the host-side venus objects and blob resources are
/// torn down implicitly when the persistent virtio context is destroyed (the
/// caller destroys the context in StopDevice and unrefs the page-table blob).
///
/// Reaching this type at all means the full bring-up succeeded: there is no
/// constructor other than [`VenusInstance::into_device`].
pub struct VenusClient {
    /// Stage-1 state. Every ring primitive lives here, so the ~40 methods below
    /// reach it through the small delegating helpers rather than by owning the
    /// fields directly.
    ring: VenusRing,
    /// venus device handle. Not `Option`: a `VenusClient` without a device is
    /// unrepresentable, which is the whole point of the typestate.
    device_id: VkDeviceId,
    /// HOST_VISIBLE|HOST_COHERENT memory type chosen during bring-up.
    memory_type_index: MemoryTypeIndex,
    /// Raw VkMemoryPropertyFlags for physical-device memory types.
    memory_type_flags: [u32; VK_MAX_MEMORY_TYPES as usize],
    memory_type_count: u32,
    /// Exact identities of live memory-blob allocations. Capacity is bounded
    /// by the transport's blob table and teardown removes an id only after the
    /// corresponding Venus free command has been accepted.
    owned_memory_blobs: Vec<VkDeviceMemoryId>,
    /// D5b render queue + command pool + per-slot command buffers, created on
    /// the first BLT present rather than at bring-up (StartDevice's stack).
    present_copy: Option<present_copy::PresentCopyObjects>,
}

impl VenusClient {
    /// The HOST_VISIBLE|HOST_COHERENT venus `memoryTypeIndex` every
    /// [`Self::allocate_memory_blob`] allocation uses — recorded into the
    /// allocation identity so cross-process openers import with the creator's
    /// exact memory type.
    pub fn memory_type_index(&self) -> u32 {
        self.memory_type_index.0
    }

    // ── Stage-1 delegates ────────────────────────────────────────────────────
    //
    // The ring primitives belong to VenusRing, which exists before any Vulkan
    // object does. These forwarders exist so the ~40 object methods below read
    // as they did — the alternative was `self.ring.` on 200 lines, which would
    // have buried the actual change.

    /// The persistent venus 3D context id all commands ride.
    fn ctx_id(&self) -> u32 {
        self.ring.ctx_id
    }

    /// The stage-1 PASSIVE proof (R614). See [`VenusRing::passive`] for why a
    /// bring-up-time token is the right provenance for a client method.
    fn passive(&self) -> PassiveLevel {
        self.ring.passive
    }

    fn submit_direct(&self, adapter: &AdapterContext, stream: &[u8]) -> Result<(), VirtioError> {
        self.ring.submit_direct(adapter, stream)
    }

    fn ring_command_noreply(
        &mut self,
        adapter: &AdapterContext,
        stream: &[u8],
    ) -> Result<(), VirtioError> {
        self.ring.ring_command_noreply(adapter, stream)
    }

    /// See [`VenusRing::ring_command_expect`]. Delegating rather than
    /// re-implementing keeps the reader's single construction site on the ring,
    /// where the publish/wait it must follow also lives.
    fn ring_command_expect(
        &mut self,
        adapter: &AdapterContext,
        stream: &[u8],
        check: ReplyCheck,
    ) -> Result<ReplyReader<'_>, VirtioError> {
        self.ring.ring_command_expect(adapter, stream, check)
    }

    /// Mint the next raw handle. Private: every caller goes through one of the
    /// typed constructors below, so a handle cannot be created without deciding
    /// what class of object it names. The counter itself lives on the ring,
    /// which is the stage that exists before any Vulkan object does.
    fn next_raw(&mut self) -> NonZeroU64 {
        self.ring.next_raw()
    }

    fn new_image_id(&mut self) -> VkImageId {
        VkImageId(self.next_raw())
    }

    fn new_memory_id(&mut self) -> VkDeviceMemoryId {
        VkDeviceMemoryId(self.next_raw())
    }

    /// Destroy a Venus `VkImage` allocated by the kernel Venus client. Best
    /// effort: allocation teardown must not wedge if the host context is already
    /// being destroyed.
    pub fn destroy_image(
        &mut self,
        adapter: &AdapterContext,
        image_id: u64,
    ) -> Result<(), VirtioError> {
        let image_id = VkImageId::from_raw(image_id).ok_or(VirtioError::DeviceError)?;
        let mut w = Writer::new();
        w.header(CMD_DESTROY_IMAGE, 0);
        w.handle(self.device_id);
        w.handle(image_id);
        w.count(false);
        self.submit_direct(adapter, w.as_slice()?)
    }

    /// Enqueue a raw Venus `vkFreeMemory` command.
    fn free_memory_object(
        &mut self,
        adapter: &AdapterContext,
        memory_id: VkDeviceMemoryId,
    ) -> Result<(), VirtioError> {
        let mut w = Writer::new();
        w.header(CMD_FREE_MEMORY, 0);
        w.handle(self.device_id);
        w.handle(memory_id);
        w.count(false); // pAllocator = NULL
        self.ring_command_noreply(adapter, w.as_slice()?)
    }

    /// Free a Venus `VkDeviceMemory`, removing its local blob identity only
    /// after the free command has been accepted by the ring.
    pub fn free_memory_blob(
        &mut self,
        adapter: &AdapterContext,
        memory_id: u64,
    ) -> Result<(), VirtioError> {
        let memory_id = VkDeviceMemoryId::from_raw(memory_id).ok_or(VirtioError::DeviceError)?;
        let owned_index = self
            .owned_memory_blobs
            .iter()
            .position(|owned| *owned == memory_id);
        self.free_memory_object(adapter, memory_id)?;
        if let Some(index) = owned_index {
            self.owned_memory_blobs.swap_remove(index);
        }
        Ok(())
    }
}
