//! Pure HWA2 admission for the direct outer executor.
//!
//! `SHARED` means an allocation may cross D3D devices/processes.  It does not
//! mean that the allocation owns an HRA1 association.  The latter is stated by
//! `RESOURCE_ASSOCIATED`, and that is the exact edge HOB1 must retain whether
//! or not the D3D resource was created with a shared-resource misc flag.

use helios_protocol::wddm::{
    HeliosWddmAllocationDescV2, HELIOS_HWA2_FLAG_RESOURCE_ASSOCIATED, HELIOS_HWA2_FLAG_STANDARD,
    HELIOS_HWA2_KIND_BUFFER, HELIOS_HWA2_KIND_IMAGE,
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
        && desc.has_flag(HELIOS_HWA2_FLAG_RESOURCE_ASSOCIATED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use helios_protocol::wddm::{
        HELIOS_HWA2_FLAG_CPU_VISIBLE, HELIOS_HWA2_FLAG_SHARED, HELIOS_HWA2_MEMORY_CPU_VISIBLE,
        HELIOS_HWA2_SWIZZLE_LINEAR,
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
    fn association_standard_kind_and_validation_remain_fail_closed() {
        let mut missing_association = associated_buffer();
        missing_association.flags &= !HELIOS_HWA2_FLAG_RESOURCE_ASSOCIATED;
        assert!(!hwa2_is_outer_execution_resource(&missing_association));

        let mut standard = associated_buffer();
        standard.flags |= HELIOS_HWA2_FLAG_STANDARD;
        assert!(!hwa2_is_outer_execution_resource(&standard));

        let mut invalid = associated_buffer();
        invalid.allocation_generation = 0;
        assert!(!hwa2_is_outer_execution_resource(&invalid));
    }
}
