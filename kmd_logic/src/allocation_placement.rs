//! Pure HWA2 placement policy shared by the WDDM allocation DDI and its tests.

use helios_protocol::{
    HeliosWddmAllocationDescV2, HELIOS_HWA2_FLAG_CPU_VISIBLE, HELIOS_HWA2_FLAG_SHARED,
    HELIOS_HWA2_SWIZZLE_OPAQUE_OPTIMAL,
};

/// Whether an ordinary HWA2 allocation may prefer the reported local-memory
/// segment instead of remaining aperture-only.
///
/// A `SHARED` resource is deliberately aperture-only. On the target, the exact
/// DWM shared/resource-associated texture completed Create/Open but dxgkrnl
/// rejected the enclosing allocation transaction when local memory was
/// preferred. Before HWA2, the equivalent UMD-owned shared/present resources
/// were also excluded from local placement after that class destabilized the
/// LogonUI/DWM path; K8's generic local-memory conversion lost the class term.
pub fn hwa2_may_prefer_local_memory(
    desc: &HeliosWddmAllocationDescV2,
    host_authoritative_backing: bool,
    local_segment_present: bool,
) -> bool {
    host_authoritative_backing
        && local_segment_present
        && desc.swizzle_class != HELIOS_HWA2_SWIZZLE_OPAQUE_OPTIMAL
        && desc.has_flag(HELIOS_HWA2_FLAG_CPU_VISIBLE)
        && !desc.has_flag(HELIOS_HWA2_FLAG_SHARED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use helios_protocol::{
        HELIOS_HWA2_FLAG_RESOURCE_ASSOCIATED, HELIOS_HWA2_SWIZZLE_LINEAR,
        HELIOS_PACKAGE_GENERATION,
    };

    fn ordinary_cpu_visible() -> HeliosWddmAllocationDescV2 {
        let mut desc = HeliosWddmAllocationDescV2::header(HELIOS_PACKAGE_GENERATION, 1);
        desc.flags = HELIOS_HWA2_FLAG_CPU_VISIBLE | HELIOS_HWA2_FLAG_RESOURCE_ASSOCIATED;
        desc.swizzle_class = HELIOS_HWA2_SWIZZLE_LINEAR;
        desc
    }

    #[test]
    fn ordinary_cpu_visible_resource_may_prefer_local_memory() {
        assert!(hwa2_may_prefer_local_memory(
            &ordinary_cpu_visible(),
            true,
            true
        ));
    }

    #[test]
    fn shared_dwm_texture_remains_aperture_only() {
        let mut desc = ordinary_cpu_visible();
        desc.flags |= HELIOS_HWA2_FLAG_SHARED;
        assert!(!hwa2_may_prefer_local_memory(&desc, true, true));
    }

    #[test]
    fn opaque_or_unbacked_resource_cannot_prefer_local_memory() {
        let mut opaque = ordinary_cpu_visible();
        opaque.swizzle_class = HELIOS_HWA2_SWIZZLE_OPAQUE_OPTIMAL;
        assert!(!hwa2_may_prefer_local_memory(&opaque, true, true));
        assert!(!hwa2_may_prefer_local_memory(
            &ordinary_cpu_visible(),
            false,
            true
        ));
        assert!(!hwa2_may_prefer_local_memory(
            &ordinary_cpu_visible(),
            true,
            false
        ));
    }
}
