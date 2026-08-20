//! Exact D3D12 resource/WDDM ownership for one UMD device generation.
//!
//! The pre-A7 implementation used a process-global `OnceLock<HashMap>` keyed
//! by an engine COM address. That made two devices in one process share one
//! identity namespace and let pointer reuse participate in lookup. The live
//! registry is now embedded in `HeliosD3D12Device`, bounded, and keyed by the
//! package token assigned before the exact vkd3d heap allocation. Engine and
//! runtime handles remain teardown/callback facts only; neither is the token.

use helios_protocol::{
    HeliosResourceAssociationV1, HELIOS_PACKAGE_GENERATION,
    HELIOS_RESOURCE_ASSOCIATION_ABI_VERSION, HELIOS_RESOURCE_ASSOCIATION_BYTES,
    HELIOS_RESOURCE_ASSOCIATION_FLAG_CPU_MAPPING, HELIOS_RESOURCE_ASSOCIATION_STRUCTURE_TYPE,
};
use std::sync::{Mutex, MutexGuard};

pub(crate) const MAX_LIVE_ALLOCATIONS: usize = 4096;

#[derive(Clone, Copy, Debug)]
pub(crate) struct IdentityGeometry {
    pub(crate) width: u64,
    pub(crate) height: u32,
    pub(crate) depth_or_array_size: u16,
    pub(crate) mip_levels: u16,
    pub(crate) sample_count: u32,
    pub(crate) dxgi_format: u32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct AllocationIdentity {
    pub(crate) engine_resource: usize,
    pub(crate) memory_offset: u64,
    pub(crate) memory_size: u64,
    pub(crate) allocation_generation: u64,
    pub(crate) h_allocation: u32,
    pub(crate) h_km_resource: u32,
    pub(crate) h_rt_resource: usize,
    pub(crate) gpu_virtual_address: u64,
    /// Page-rounded mapping extent owned by this UMD for a standalone internal
    /// allocation. Zero for runtime-resource-associated allocations.
    pub(crate) gpuva_reserved_bytes: u64,
    pub(crate) standalone: bool,
    pub(crate) device_generation: u64,
    pub(crate) outer_allocation_token: u64,
    pub(crate) geometry: IdentityGeometry,
    pub(crate) pitch: u32,
    pub(crate) heap_flags: u32,
    /// Exact outer context which most recently accepted a batch naming this
    /// allocation. Zero means the allocation was never host-materialised.
    pub(crate) last_context_generation: u64,
    /// Set before the owning resource/heap COM graph is dropped. A pending
    /// identity resolves only for its one terminal teardown scope.
    pub(crate) teardown_pending: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IdentityRefusal {
    ZeroDeviceGeneration,
    ZeroBytes,
    TokenOverflow,
    LiveSetFull,
    DuplicateToken,
    DuplicateResource,
    ForeignDeviceGeneration,
    ZeroAllocation,
    ZeroAllocationGeneration,
    ZeroGpuVirtualAddress,
    MissingToken,
    RangeOverflow,
    RangeOutOfBounds,
    UseAfterDestroy,
    TeardownAlreadyPending,
    TeardownNotPending,
    ResourceMismatch,
    ContextGenerationZero,
    InvalidOwnership,
    CpuBackingMismatch,
}

struct AllocationEntry {
    identity: AllocationIdentity,
    cpu_backing: Option<helios_umd_common::cpu_backing::CpuBacking>,
}

pub(crate) struct RetiredAllocation {
    pub(crate) identity: AllocationIdentity,
    cpu_backing: Option<helios_umd_common::cpu_backing::CpuBacking>,
}

impl RetiredAllocation {
    /// Release the CPU view only after dxgkrnl has released pSystemMem. A
    /// failed deallocation intentionally leaks the view rather than leaving a
    /// live WDDM allocation pointing at freed process memory.
    pub(crate) fn complete(mut self, deallocated: bool) {
        if !deallocated {
            if let Some(backing) = self.cpu_backing.take() {
                backing.leak();
            }
        }
    }
}

pub(crate) struct IdentityRegistry {
    next_token: u64,
    entries: Vec<AllocationEntry>,
}

impl Drop for IdentityRegistry {
    fn drop(&mut self) {
        for mut entry in self.entries.drain(..) {
            if let Some(backing) = entry.cpu_backing.take() {
                backing.leak();
            }
        }
    }
}

impl IdentityRegistry {
    pub(crate) fn new() -> Self {
        Self {
            next_token: 1,
            entries: Vec::new(),
        }
    }

    /// Reserve a never-reused token before vkd3d enters vkAllocateMemory. A
    /// failed later create consumes the number but publishes no live entry.
    pub(crate) fn reserve(
        &mut self,
        device_generation: u64,
        bytes: u64,
        cpu_mapping: *mut core::ffi::c_void,
    ) -> Result<HeliosResourceAssociationV1, IdentityRefusal> {
        use IdentityRefusal as R;
        if device_generation == 0 {
            return Err(R::ZeroDeviceGeneration);
        }
        if bytes == 0 {
            return Err(R::ZeroBytes);
        }
        let token = self.next_token;
        if token == 0 || token == u64::MAX {
            return Err(R::TokenOverflow);
        }
        self.next_token = token + 1;
        Ok(HeliosResourceAssociationV1 {
            s_type: HELIOS_RESOURCE_ASSOCIATION_STRUCTURE_TYPE,
            struct_bytes: HELIOS_RESOURCE_ASSOCIATION_BYTES,
            p_next: core::ptr::null(),
            abi_version: HELIOS_RESOURCE_ASSOCIATION_ABI_VERSION,
            reserved: 0,
            package_generation: HELIOS_PACKAGE_GENERATION,
            device_generation,
            outer_allocation_token: token,
            outer_allocation_bytes: bytes,
            cpu_mapping,
            association_flags: if cpu_mapping.is_null() {
                0
            } else {
                HELIOS_RESOURCE_ASSOCIATION_FLAG_CPU_MAPPING
            },
            reserved1: 0,
        })
    }

    pub(crate) fn commit(
        &mut self,
        identity: AllocationIdentity,
        cpu_backing: Option<helios_umd_common::cpu_backing::CpuBacking>,
    ) -> Result<
        (),
        (
            IdentityRefusal,
            Option<helios_umd_common::cpu_backing::CpuBacking>,
        ),
    > {
        use IdentityRefusal as R;
        if self.entries.len() >= MAX_LIVE_ALLOCATIONS {
            return Err((R::LiveSetFull, cpu_backing));
        }
        if identity.device_generation == 0 {
            return Err((R::ZeroDeviceGeneration, cpu_backing));
        }
        if identity.outer_allocation_token == 0 {
            return Err((R::MissingToken, cpu_backing));
        }
        if identity.h_allocation == 0 {
            return Err((R::ZeroAllocation, cpu_backing));
        }
        if identity.allocation_generation == 0 {
            return Err((R::ZeroAllocationGeneration, cpu_backing));
        }
        if identity.gpu_virtual_address == 0 {
            return Err((R::ZeroGpuVirtualAddress, cpu_backing));
        }
        if identity.standalone {
            if identity.engine_resource != 0
                || identity.h_rt_resource != 0
                || identity.gpuva_reserved_bytes < identity.memory_size
                || identity.gpuva_reserved_bytes == 0
            {
                return Err((R::InvalidOwnership, cpu_backing));
            }
        } else if identity.engine_resource == 0
            || identity.h_rt_resource == 0
            || identity.gpuva_reserved_bytes != 0
        {
            return Err((R::InvalidOwnership, cpu_backing));
        }
        if cpu_backing.as_ref().is_some_and(|backing| {
            u64::try_from(backing.bytes()).ok() != Some(identity.memory_size)
        }) {
            return Err((R::CpuBackingMismatch, cpu_backing));
        }
        if self
            .entries
            .iter()
            .any(|entry| entry.identity.outer_allocation_token == identity.outer_allocation_token)
        {
            return Err((R::DuplicateToken, cpu_backing));
        }
        if identity.engine_resource != 0
            && self
                .entries
                .iter()
                .any(|entry| entry.identity.engine_resource == identity.engine_resource)
        {
            return Err((R::DuplicateResource, cpu_backing));
        }
        self.entries.push(AllocationEntry {
            identity,
            cpu_backing,
        });
        Ok(())
    }

    /// Arm a token-bearing allocation from the immutable engine teardown edge.
    /// Resource-associated allocations have already been armed by their DDI
    /// destroy path; accepting that exact state is idempotence, not a fallback.
    pub(crate) fn arm_teardown_by_token(
        &mut self,
        device_generation: u64,
        token: u64,
    ) -> Result<AllocationIdentity, IdentityRefusal> {
        use IdentityRefusal as R;
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.identity.outer_allocation_token == token)
            .ok_or(R::MissingToken)?;
        if entry.identity.device_generation != device_generation {
            return Err(R::ForeignDeviceGeneration);
        }
        if !entry.identity.teardown_pending {
            entry.identity.teardown_pending = true;
        }
        Ok(entry.identity)
    }

    pub(crate) fn resolve_token(
        &self,
        device_generation: u64,
        token: u64,
        byte_offset: u64,
        byte_length: u64,
        terminal_token: Option<u64>,
    ) -> Result<AllocationIdentity, IdentityRefusal> {
        use IdentityRefusal as R;
        if device_generation == 0 {
            return Err(R::ZeroDeviceGeneration);
        }
        let identity = self
            .entries
            .iter()
            .find(|entry| entry.identity.outer_allocation_token == token)
            .map(|entry| entry.identity)
            .ok_or(R::MissingToken)?;
        if identity.device_generation != device_generation {
            return Err(R::ForeignDeviceGeneration);
        }
        if identity.teardown_pending && terminal_token != Some(token) {
            return Err(R::UseAfterDestroy);
        }
        let end = byte_offset
            .checked_add(byte_length)
            .ok_or(R::RangeOverflow)?;
        if byte_length == 0 || end > identity.memory_size {
            return Err(R::RangeOutOfBounds);
        }
        Ok(identity)
    }

    pub(crate) fn note_context(
        &mut self,
        device_generation: u64,
        tokens: &[u64],
        context_generation: u64,
        terminal_token: Option<u64>,
    ) -> Result<(), IdentityRefusal> {
        use IdentityRefusal as R;
        if context_generation == 0 {
            return Err(R::ContextGenerationZero);
        }
        for &token in tokens {
            let entry = self
                .entries
                .iter_mut()
                .find(|entry| entry.identity.outer_allocation_token == token)
                .ok_or(R::MissingToken)?;
            if entry.identity.device_generation != device_generation {
                return Err(R::ForeignDeviceGeneration);
            }
            if entry.identity.teardown_pending && terminal_token != Some(token) {
                return Err(R::UseAfterDestroy);
            }
            entry.identity.last_context_generation = context_generation;
        }
        Ok(())
    }

    pub(crate) fn arm_teardown(
        &mut self,
        engine_resource: usize,
        device_generation: u64,
        token: u64,
    ) -> Result<AllocationIdentity, IdentityRefusal> {
        use IdentityRefusal as R;
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.identity.outer_allocation_token == token)
            .ok_or(R::MissingToken)?;
        if entry.identity.device_generation != device_generation {
            return Err(R::ForeignDeviceGeneration);
        }
        if entry.identity.engine_resource != engine_resource {
            return Err(R::ResourceMismatch);
        }
        if entry.identity.teardown_pending {
            return Err(R::TeardownAlreadyPending);
        }
        entry.identity.teardown_pending = true;
        Ok(entry.identity)
    }

    pub(crate) fn pending_teardown(
        &self,
        device_generation: u64,
        token: u64,
    ) -> Result<AllocationIdentity, IdentityRefusal> {
        use IdentityRefusal as R;
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.identity.outer_allocation_token == token)
            .ok_or(R::MissingToken)?;
        if entry.identity.device_generation != device_generation {
            return Err(R::ForeignDeviceGeneration);
        }
        if !entry.identity.teardown_pending {
            return Err(R::TeardownNotPending);
        }
        Ok(entry.identity)
    }

    pub(crate) fn retire_teardown(
        &mut self,
        device_generation: u64,
        token: u64,
    ) -> Result<RetiredAllocation, IdentityRefusal> {
        let identity = self.pending_teardown(device_generation, token)?;
        let index = self
            .entries
            .iter()
            .position(|entry| entry.identity.outer_allocation_token == token)
            .ok_or(IdentityRefusal::MissingToken)?;
        let removed = self.entries.swap_remove(index);
        debug_assert_eq!(removed.identity.engine_resource, identity.engine_resource);
        Ok(RetiredAllocation {
            identity: removed.identity,
            cpu_backing: removed.cpu_backing,
        })
    }
}

pub(crate) fn lock(registry: &Mutex<IdentityRegistry>) -> MutexGuard<'_, IdentityRegistry> {
    registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
