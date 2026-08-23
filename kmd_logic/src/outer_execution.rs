//! Pure HWA2 admission for the direct outer executor.
//!
//! `SHARED` means an allocation may cross D3D devices/processes.  It does not
//! mean that the allocation owns an HRA1 association; `RESOURCE_ASSOCIATED`
//! states that, and neither flag decides admission here.  What decides it is
//! whether a HOB1 batch may legitimately NAME the allocation in its use list —
//! and DXVK's internal allocator chunks (plain unassociated buffers backing
//! every vkAllocateMemory) are named by every content batch.  Requiring
//! `RESOURCE_ASSOCIATED` left those opens without an execution edge, and the
//! first content batch of every device died on arm 11 / UseMissingOrForeign
//! (measured 2026-08-24: Nr2UseArm=11, Nr2UseLen=4 MiB, the readback probe and
//! every dwm generation alike).  Only KMD-authored `STANDARD` allocations stay
//! out: batches never name them.

use helios_protocol::wddm::{
    HeliosWddmAllocationDescV2, HELIOS_HWA2_FLAG_STANDARD, HELIOS_HWA2_KIND_BUFFER,
    HELIOS_HWA2_KIND_IMAGE,
};
use helios_protocol::HELIOS_PACKAGE_GENERATION;

/// Whether one canonical HWA2 output requires an outer execution binding.
///
/// The caller still proves canonical allocation identity and live backing.
/// This predicate owns only the immutable HWA2 shape decision, so open-time
/// binding and execution-time projection cannot drift onto different flag
/// meanings.
pub fn hwa2_is_outer_execution_resource(desc: &HeliosWddmAllocationDescV2) -> bool {
    desc.validate_create_output(HELIOS_PACKAGE_GENERATION)
        .is_ok()
        && matches!(
            desc.allocation_kind,
            HELIOS_HWA2_KIND_BUFFER | HELIOS_HWA2_KIND_IMAGE
        )
        && !desc.has_flag(HELIOS_HWA2_FLAG_STANDARD)
}

#[cfg(test)]
mod tests {
    use super::*;
    use helios_protocol::wddm::{
        HELIOS_HWA2_FLAG_CPU_VISIBLE, HELIOS_HWA2_FLAG_RESOURCE_ASSOCIATED,
        HELIOS_HWA2_FLAG_SHARED, HELIOS_HWA2_MEMORY_CPU_VISIBLE, HELIOS_HWA2_SWIZZLE_LINEAR,
    };

    fn associated_buffer() -> HeliosWddmAllocationDescV2 {
        let mut desc = HeliosWddmAllocationDescV2::header(HELIOS_PACKAGE_GENERATION, 0x1_0000_0001);
        desc.byte_size = 4096;
        desc.allocation_kind = HELIOS_HWA2_KIND_BUFFER;
        desc.flags = HELIOS_HWA2_FLAG_CPU_VISIBLE | HELIOS_HWA2_FLAG_RESOURCE_ASSOCIATED;
        desc.swizzle_class = HELIOS_HWA2_SWIZZLE_LINEAR;
        desc.memory_class = HELIOS_HWA2_MEMORY_CPU_VISIBLE;
        assert!(desc
            .validate_create_output(HELIOS_PACKAGE_GENERATION)
            .is_ok());
        desc
    }

    #[test]
    fn ordinary_resource_association_does_not_require_external_sharing() {
        let desc = associated_buffer();
        assert_eq!(desc.flags, 0x300);
        assert!(hwa2_is_outer_execution_resource(&desc));
    }

    #[test]
    fn externally_shared_resource_association_remains_admitted() {
        let mut desc = associated_buffer();
        desc.flags |= HELIOS_HWA2_FLAG_SHARED;
        assert!(hwa2_is_outer_execution_resource(&desc));
    }

    #[test]
    fn unassociated_internal_chunk_is_admitted() {
        // The DXVK-internal allocator chunk shape: plain buffer, no HRA1
        // association. Batches name it, so it needs the execution edge.
        let mut internal = associated_buffer();
        internal.flags &= !HELIOS_HWA2_FLAG_RESOURCE_ASSOCIATED;
        assert!(hwa2_is_outer_execution_resource(&internal));
    }

    #[test]
    fn association_standard_kind_and_validation_remain_fail_closed() {
        let mut standard = associated_buffer();
        standard.flags |= HELIOS_HWA2_FLAG_STANDARD;
        assert!(!hwa2_is_outer_execution_resource(&standard));

        let mut invalid = associated_buffer();
        invalid.allocation_generation = 0;
        assert!(!hwa2_is_outer_execution_resource(&invalid));
    }
}
