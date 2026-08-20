use core::ffi::c_void;

pub const HELIOS_RESOURCE_ASSOCIATION_STRUCTURE_TYPE: u32 = 0x4852_4131;
pub const HELIOS_RESOURCE_ASSOCIATION_ABI_VERSION: u32 = 1;
pub const HELIOS_RESOURCE_ASSOCIATION_BYTES: u32 = 72;
pub const HELIOS_RESOURCE_ASSOCIATION_FLAG_CPU_MAPPING: u32 = 1 << 0;
pub const HELIOS_RESOURCE_ASSOCIATION_FLAG_MASK: u32 = HELIOS_RESOURCE_ASSOCIATION_FLAG_CPU_MAPPING;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeliosResourceAssociationV1 {
    pub s_type: u32,
    pub struct_bytes: u32,
    pub p_next: *const c_void,
    pub abi_version: u32,
    pub reserved: u32,
    pub package_generation: u64,
    pub device_generation: u64,
    pub outer_allocation_token: u64,
    pub outer_allocation_bytes: u64,
    /// Optional process-local CPU view of the exact outer allocation backing.
    ///
    /// This value is never an identity or lookup key.  It is consumed only by
    /// the synchronous `vkMapMemory` path of the exact allocation whose token
    /// appears above, and remains owned by the outer UMD until reverse teardown.
    pub cpu_mapping: *mut c_void,
    pub association_flags: u32,
    pub reserved1: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeliosResourceAssociationRefusal {
    StructureType,
    StructBytes,
    AbiVersion,
    Reserved,
    PackageGeneration,
    DeviceGeneration,
    OuterAllocationToken,
    OuterAllocationBytes,
    AssociationFlags,
    CpuMapping,
    Reserved1,
}

impl HeliosResourceAssociationV1 {
    pub const fn validate(
        &self,
        expected_package_generation: u64,
        expected_device_generation: u64,
    ) -> Result<(), HeliosResourceAssociationRefusal> {
        use HeliosResourceAssociationRefusal as R;

        if self.s_type != HELIOS_RESOURCE_ASSOCIATION_STRUCTURE_TYPE {
            return Err(R::StructureType);
        }
        if self.struct_bytes != HELIOS_RESOURCE_ASSOCIATION_BYTES {
            return Err(R::StructBytes);
        }
        if self.abi_version != HELIOS_RESOURCE_ASSOCIATION_ABI_VERSION {
            return Err(R::AbiVersion);
        }
        if self.reserved != 0 {
            return Err(R::Reserved);
        }
        if self.package_generation == 0 || self.package_generation != expected_package_generation {
            return Err(R::PackageGeneration);
        }
        if expected_device_generation == 0
            || self.device_generation == 0
            || self.device_generation != expected_device_generation
        {
            return Err(R::DeviceGeneration);
        }
        if self.outer_allocation_token == 0 {
            return Err(R::OuterAllocationToken);
        }
        if self.outer_allocation_bytes == 0 {
            return Err(R::OuterAllocationBytes);
        }
        if self.association_flags & !HELIOS_RESOURCE_ASSOCIATION_FLAG_MASK != 0 {
            return Err(R::AssociationFlags);
        }
        if (self.association_flags & HELIOS_RESOURCE_ASSOCIATION_FLAG_CPU_MAPPING != 0)
            != !self.cpu_mapping.is_null()
        {
            return Err(R::CpuMapping);
        }
        if self.reserved1 != 0 {
            return Err(R::Reserved1);
        }
        Ok(())
    }
}

const _: () = {
    assert!(core::mem::size_of::<HeliosResourceAssociationV1>() == 72);
    assert!(core::mem::align_of::<HeliosResourceAssociationV1>() == 8);
    assert!(core::mem::offset_of!(HeliosResourceAssociationV1, s_type) == 0);
    assert!(core::mem::offset_of!(HeliosResourceAssociationV1, struct_bytes) == 4);
    assert!(core::mem::offset_of!(HeliosResourceAssociationV1, p_next) == 8);
    assert!(core::mem::offset_of!(HeliosResourceAssociationV1, abi_version) == 16);
    assert!(core::mem::offset_of!(HeliosResourceAssociationV1, reserved) == 20);
    assert!(core::mem::offset_of!(HeliosResourceAssociationV1, package_generation) == 24);
    assert!(core::mem::offset_of!(HeliosResourceAssociationV1, device_generation) == 32);
    assert!(core::mem::offset_of!(HeliosResourceAssociationV1, outer_allocation_token) == 40);
    assert!(core::mem::offset_of!(HeliosResourceAssociationV1, outer_allocation_bytes) == 48);
    assert!(core::mem::offset_of!(HeliosResourceAssociationV1, cpu_mapping) == 56);
    assert!(core::mem::offset_of!(HeliosResourceAssociationV1, association_flags) == 64);
    assert!(core::mem::offset_of!(HeliosResourceAssociationV1, reserved1) == 68);
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HELIOS_PACKAGE_GENERATION;

    const DEVICE_GENERATION: u64 = 9;

    fn valid() -> HeliosResourceAssociationV1 {
        HeliosResourceAssociationV1 {
            s_type: HELIOS_RESOURCE_ASSOCIATION_STRUCTURE_TYPE,
            struct_bytes: HELIOS_RESOURCE_ASSOCIATION_BYTES,
            p_next: core::ptr::null(),
            abi_version: HELIOS_RESOURCE_ASSOCIATION_ABI_VERSION,
            reserved: 0,
            package_generation: HELIOS_PACKAGE_GENERATION,
            device_generation: DEVICE_GENERATION,
            outer_allocation_token: 1,
            outer_allocation_bytes: 4096,
            cpu_mapping: core::ptr::null_mut(),
            association_flags: 0,
            reserved1: 0,
        }
    }

    #[test]
    fn validates_exact_association() {
        assert_eq!(
            valid().validate(HELIOS_PACKAGE_GENERATION, DEVICE_GENERATION),
            Ok(())
        );
    }

    #[test]
    fn rejects_every_identity_and_shape_mismatch() {
        let mut cases = [(valid(), HeliosResourceAssociationRefusal::StructureType); 11];
        cases[0].0.s_type ^= 1;
        cases[1] = (valid(), HeliosResourceAssociationRefusal::StructBytes);
        cases[1].0.struct_bytes += 8;
        cases[2] = (valid(), HeliosResourceAssociationRefusal::AbiVersion);
        cases[2].0.abi_version += 1;
        cases[3] = (valid(), HeliosResourceAssociationRefusal::Reserved);
        cases[3].0.reserved = 1;
        cases[4] = (valid(), HeliosResourceAssociationRefusal::PackageGeneration);
        cases[4].0.package_generation += 1;
        cases[5] = (valid(), HeliosResourceAssociationRefusal::DeviceGeneration);
        cases[5].0.device_generation += 1;
        cases[6] = (
            valid(),
            HeliosResourceAssociationRefusal::OuterAllocationToken,
        );
        cases[6].0.outer_allocation_token = 0;
        cases[7] = (
            valid(),
            HeliosResourceAssociationRefusal::OuterAllocationBytes,
        );
        cases[7].0.outer_allocation_bytes = 0;
        cases[8] = (valid(), HeliosResourceAssociationRefusal::AssociationFlags);
        cases[8].0.association_flags = 2;
        cases[9] = (valid(), HeliosResourceAssociationRefusal::CpuMapping);
        cases[9].0.association_flags = HELIOS_RESOURCE_ASSOCIATION_FLAG_CPU_MAPPING;
        cases[10] = (valid(), HeliosResourceAssociationRefusal::Reserved1);
        cases[10].0.reserved1 = 1;

        for (record, expected) in cases {
            assert_eq!(
                record.validate(HELIOS_PACKAGE_GENERATION, DEVICE_GENERATION),
                Err(expected)
            );
        }
        assert_eq!(
            valid().validate(HELIOS_PACKAGE_GENERATION, 0),
            Err(HeliosResourceAssociationRefusal::DeviceGeneration)
        );

        let mut mapped = valid();
        mapped.cpu_mapping = core::ptr::dangling_mut::<u8>().cast();
        mapped.association_flags = HELIOS_RESOURCE_ASSOCIATION_FLAG_CPU_MAPPING;
        assert_eq!(
            mapped.validate(HELIOS_PACKAGE_GENERATION, DEVICE_GENERATION),
            Ok(())
        );
    }
}
