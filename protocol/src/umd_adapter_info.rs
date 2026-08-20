//! Fixed UMD-private adapter bootstrap record.
//!
//! This record crosses only the existing WDDM
//! `pfnQueryAdapterInfoCb`/`DXGKQAITYPE_UMDRIVERPRIVATE` edge.  It lets the two
//! package UMDs obtain the exact `DXGK_START_INFO::AdapterLuid` and lifecycle
//! generation before constructing the sole direct A5 translator instance.  It
//! is not a resource identity, registration service, or host wire record.

pub const HELIOS_UMD_ADAPTER_INFO_MAGIC: u32 = 0x3149_4148;
pub const HELIOS_UMD_ADAPTER_INFO_ABI_VERSION: u32 = 1;
pub const HELIOS_UMD_ADAPTER_INFO_BYTES: u32 = 48;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeliosUmdAdapterInfoV1 {
    pub magic: u32,
    pub struct_bytes: u32,
    pub abi_version: u32,
    pub reserved0: u32,
    pub package_generation: u64,
    pub adapter_generation: u64,
    pub adapter_luid: i64,
    pub reserved1: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeliosUmdAdapterInfoRefusal {
    Magic,
    StructBytes,
    AbiVersion,
    Reserved,
    PackageGeneration,
    AdapterGeneration,
    AdapterLuid,
}

impl HeliosUmdAdapterInfoV1 {
    pub const fn query(package_generation: u64) -> Self {
        Self {
            magic: HELIOS_UMD_ADAPTER_INFO_MAGIC,
            struct_bytes: HELIOS_UMD_ADAPTER_INFO_BYTES,
            abi_version: HELIOS_UMD_ADAPTER_INFO_ABI_VERSION,
            reserved0: 0,
            package_generation,
            adapter_generation: 0,
            adapter_luid: 0,
            reserved1: 0,
        }
    }

    pub const fn validate_query(
        &self,
        expected_package_generation: u64,
    ) -> Result<(), HeliosUmdAdapterInfoRefusal> {
        if let Err(refusal) = self.validate_shape(expected_package_generation) {
            return Err(refusal);
        }
        if self.adapter_generation != 0 {
            return Err(HeliosUmdAdapterInfoRefusal::AdapterGeneration);
        }
        if self.adapter_luid != 0 {
            return Err(HeliosUmdAdapterInfoRefusal::AdapterLuid);
        }
        Ok(())
    }

    pub const fn validate_reply(
        &self,
        expected_package_generation: u64,
    ) -> Result<(), HeliosUmdAdapterInfoRefusal> {
        if let Err(refusal) = self.validate_shape(expected_package_generation) {
            return Err(refusal);
        }
        if self.adapter_generation == 0 {
            return Err(HeliosUmdAdapterInfoRefusal::AdapterGeneration);
        }
        if self.adapter_luid == 0 {
            return Err(HeliosUmdAdapterInfoRefusal::AdapterLuid);
        }
        Ok(())
    }

    const fn validate_shape(
        &self,
        expected_package_generation: u64,
    ) -> Result<(), HeliosUmdAdapterInfoRefusal> {
        use HeliosUmdAdapterInfoRefusal as R;

        if self.magic != HELIOS_UMD_ADAPTER_INFO_MAGIC {
            return Err(R::Magic);
        }
        if self.struct_bytes != HELIOS_UMD_ADAPTER_INFO_BYTES {
            return Err(R::StructBytes);
        }
        if self.abi_version != HELIOS_UMD_ADAPTER_INFO_ABI_VERSION {
            return Err(R::AbiVersion);
        }
        if self.reserved0 != 0 || self.reserved1 != 0 {
            return Err(R::Reserved);
        }
        if expected_package_generation == 0
            || self.package_generation != expected_package_generation
        {
            return Err(R::PackageGeneration);
        }
        Ok(())
    }
}

const _: () = {
    assert!(HELIOS_UMD_ADAPTER_INFO_MAGIC.to_le_bytes()[0] == b'H');
    assert!(HELIOS_UMD_ADAPTER_INFO_MAGIC.to_le_bytes()[1] == b'A');
    assert!(HELIOS_UMD_ADAPTER_INFO_MAGIC.to_le_bytes()[2] == b'I');
    assert!(HELIOS_UMD_ADAPTER_INFO_MAGIC.to_le_bytes()[3] == b'1');
    assert!(core::mem::size_of::<HeliosUmdAdapterInfoV1>() == 48);
    assert!(core::mem::align_of::<HeliosUmdAdapterInfoV1>() == 8);
    assert!(core::mem::offset_of!(HeliosUmdAdapterInfoV1, magic) == 0);
    assert!(core::mem::offset_of!(HeliosUmdAdapterInfoV1, struct_bytes) == 4);
    assert!(core::mem::offset_of!(HeliosUmdAdapterInfoV1, abi_version) == 8);
    assert!(core::mem::offset_of!(HeliosUmdAdapterInfoV1, reserved0) == 12);
    assert!(core::mem::offset_of!(HeliosUmdAdapterInfoV1, package_generation) == 16);
    assert!(core::mem::offset_of!(HeliosUmdAdapterInfoV1, adapter_generation) == 24);
    assert!(core::mem::offset_of!(HeliosUmdAdapterInfoV1, adapter_luid) == 32);
    assert!(core::mem::offset_of!(HeliosUmdAdapterInfoV1, reserved1) == 40);
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HELIOS_PACKAGE_GENERATION;

    #[test]
    fn query_and_reply_are_disjoint_and_fail_closed() {
        let mut record = HeliosUmdAdapterInfoV1::query(HELIOS_PACKAGE_GENERATION);
        assert_eq!(record.validate_query(HELIOS_PACKAGE_GENERATION), Ok(()));
        assert_eq!(
            record.validate_reply(HELIOS_PACKAGE_GENERATION),
            Err(HeliosUmdAdapterInfoRefusal::AdapterGeneration)
        );

        record.adapter_generation = 7;
        record.adapter_luid = 0x1234;
        assert_eq!(record.validate_reply(HELIOS_PACKAGE_GENERATION), Ok(()));
        assert_eq!(
            record.validate_query(HELIOS_PACKAGE_GENERATION),
            Err(HeliosUmdAdapterInfoRefusal::AdapterGeneration)
        );
    }

    #[test]
    fn rejects_shape_generation_identity_and_reserved_mutations() {
        let valid = HeliosUmdAdapterInfoV1 {
            adapter_generation: 1,
            adapter_luid: 1,
            ..HeliosUmdAdapterInfoV1::query(HELIOS_PACKAGE_GENERATION)
        };
        let mut cases = [(valid, HeliosUmdAdapterInfoRefusal::Magic); 7];
        cases[0].0.magic ^= 1;
        cases[1] = (valid, HeliosUmdAdapterInfoRefusal::StructBytes);
        cases[1].0.struct_bytes += 8;
        cases[2] = (valid, HeliosUmdAdapterInfoRefusal::AbiVersion);
        cases[2].0.abi_version += 1;
        cases[3] = (valid, HeliosUmdAdapterInfoRefusal::Reserved);
        cases[3].0.reserved1 = 1;
        cases[4] = (valid, HeliosUmdAdapterInfoRefusal::PackageGeneration);
        cases[4].0.package_generation += 1;
        cases[5] = (valid, HeliosUmdAdapterInfoRefusal::AdapterGeneration);
        cases[5].0.adapter_generation = 0;
        cases[6] = (valid, HeliosUmdAdapterInfoRefusal::AdapterLuid);
        cases[6].0.adapter_luid = 0;

        for (record, expected) in cases {
            assert_eq!(
                record.validate_reply(HELIOS_PACKAGE_GENERATION),
                Err(expected)
            );
        }
    }
}
