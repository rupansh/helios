//! Core-0116 native-fence private driver data (HNF1).
//!
//! HNF1 is the immutable 64-byte object-associated payload exchanged by the
//! D3D12 runtime, the UMD, and the KMD while creating or opening one native
//! fence.  It is not an object identifier or a registry key: object identity is
//! exclusively the OS-delivered native-fence handles, while this record carries
//! package/type/provenance validation and one KMD-assigned diagnostic
//! generation.

use bytemuck::{Pod, Zeroable};

/// `'HNF1'` in little-endian byte order.
pub const HELIOS_HNF1_MAGIC: u32 = 0x3146_4e48;
/// HNF1 ABI version.
pub const HELIOS_HNF1_ABI_VERSION: u16 = 1;
/// The fixed `D3DDDI_NATIVE_FENCE_PDD_SIZE` payload length.
pub const HELIOS_HNF1_SIZE: usize = 64;

/// HNF1 flag bit 0: the native object is shareable.
pub const HELIOS_HNF1_FLAG_SHARED: u32 = 1 << 0;
/// Every other flag bit, including cross-adapter, is reserved.
pub const HELIOS_HNF1_FLAGS_RESERVED_MASK: u32 = !HELIOS_HNF1_FLAG_SHARED;

/// `D3DDDI_NATIVEFENCE_TYPE_DEFAULT`.
pub const HELIOS_NATIVE_FENCE_TYPE_DEFAULT: u32 = 0;
/// `D3DDDI_NATIVEFENCE_TYPE_INTRA_GPU` (not admitted by this generation).
pub const HELIOS_NATIVE_FENCE_TYPE_INTRA_GPU: u32 = 1;

/// Exact pointer-free HNF1 wire record.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct HeliosNativeFencePddV1 {
    pub magic: u32,
    pub abi_version: u16,
    pub struct_size: u16,
    pub package_generation: u64,
    pub object_generation: u64,
    pub native_type: u32,
    pub flags: u32,
    pub adapter_luid: i64,
    pub reserved: [u8; 24],
}

/// Why an HNF1 payload was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeliosNativeFencePddReject {
    Magic,
    AbiVersion,
    StructSize,
    PackageGeneration,
    ObjectGenerationNotZero,
    NativeType,
    NativeTypeMismatch,
    Flags,
    AdapterLuid,
    Reserved,
}

impl HeliosNativeFencePddV1 {
    /// Build the UMD input form. The KMD owns `object_generation` and may fill
    /// `adapter_luid`, so both are zero on create input.
    pub const fn create_input(package_generation: u64, native_type: u32, flags: u32) -> Self {
        Self {
            magic: HELIOS_HNF1_MAGIC,
            abi_version: HELIOS_HNF1_ABI_VERSION,
            struct_size: HELIOS_HNF1_SIZE as u16,
            package_generation,
            object_generation: 0,
            native_type,
            flags,
            adapter_luid: 0,
            reserved: [0; 24],
        }
    }

    fn validate_header(&self, package_generation: u64) -> Result<(), HeliosNativeFencePddReject> {
        use HeliosNativeFencePddReject as R;
        if self.magic != HELIOS_HNF1_MAGIC {
            return Err(R::Magic);
        }
        if self.abi_version != HELIOS_HNF1_ABI_VERSION {
            return Err(R::AbiVersion);
        }
        if self.struct_size as usize != HELIOS_HNF1_SIZE {
            return Err(R::StructSize);
        }
        if package_generation == 0 || self.package_generation != package_generation {
            return Err(R::PackageGeneration);
        }
        if self.flags & HELIOS_HNF1_FLAGS_RESERVED_MASK != 0 {
            return Err(R::Flags);
        }
        if self.reserved != [0; 24] {
            return Err(R::Reserved);
        }
        Ok(())
    }

    /// Validate UMD input to native-fence creation.
    pub fn validate_create_input(
        &self,
        package_generation: u64,
        adapter_luid: i64,
        ddi_native_type: u32,
    ) -> Result<(), HeliosNativeFencePddReject> {
        use HeliosNativeFencePddReject as R;
        self.validate_header(package_generation)?;
        if self.object_generation != 0 {
            return Err(R::ObjectGenerationNotZero);
        }
        if !native_type_is_admitted(self.native_type) {
            return Err(R::NativeType);
        }
        if self.native_type != ddi_native_type {
            return Err(R::NativeTypeMismatch);
        }
        if self.adapter_luid != 0 && self.adapter_luid != adapter_luid {
            return Err(R::AdapterLuid);
        }
        Ok(())
    }

    /// Validate the KMD-returned create/open form retained by the UMD.
    pub fn validate_returned(
        &self,
        package_generation: u64,
        adapter_luid: i64,
        native_type: u32,
        flags: u32,
    ) -> Result<(), HeliosNativeFencePddReject> {
        use HeliosNativeFencePddReject as R;
        self.validate_header(package_generation)?;
        if self.object_generation == 0 {
            return Err(R::ObjectGenerationNotZero);
        }
        if !native_type_is_admitted(self.native_type) {
            return Err(R::NativeType);
        }
        if self.native_type != native_type {
            return Err(R::NativeTypeMismatch);
        }
        if self.flags != flags {
            return Err(R::Flags);
        }
        if self.adapter_luid != adapter_luid {
            return Err(R::AdapterLuid);
        }
        Ok(())
    }

    /// Encode the record exactly as a 64-byte callback payload.
    pub fn to_bytes(self) -> [u8; HELIOS_HNF1_SIZE] {
        bytemuck::cast(self)
    }

    /// Decode an exact callback payload without assuming alignment.
    pub fn from_bytes(bytes: &[u8; HELIOS_HNF1_SIZE]) -> Self {
        bytemuck::pod_read_unaligned(bytes)
    }
}

/// This package implements traditional kernel-queue DEFAULT native fences
/// only. INTRA_GPU requires a separate storage/log path and is fail-closed.
pub const fn native_type_is_admitted(native_type: u32) -> bool {
    native_type == HELIOS_NATIVE_FENCE_TYPE_DEFAULT
}

const _: () = {
    assert!(core::mem::size_of::<HeliosNativeFencePddV1>() == HELIOS_HNF1_SIZE);
    assert!(core::mem::align_of::<HeliosNativeFencePddV1>() == 8);
    assert!(core::mem::offset_of!(HeliosNativeFencePddV1, magic) == 0);
    assert!(core::mem::offset_of!(HeliosNativeFencePddV1, abi_version) == 4);
    assert!(core::mem::offset_of!(HeliosNativeFencePddV1, struct_size) == 6);
    assert!(core::mem::offset_of!(HeliosNativeFencePddV1, package_generation) == 8);
    assert!(core::mem::offset_of!(HeliosNativeFencePddV1, object_generation) == 16);
    assert!(core::mem::offset_of!(HeliosNativeFencePddV1, native_type) == 24);
    assert!(core::mem::offset_of!(HeliosNativeFencePddV1, flags) == 28);
    assert!(core::mem::offset_of!(HeliosNativeFencePddV1, adapter_luid) == 32);
    assert!(core::mem::offset_of!(HeliosNativeFencePddV1, reserved) == 40);
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HELIOS_PACKAGE_GENERATION;

    #[test]
    fn create_and_returned_forms_are_distinct() {
        let input = HeliosNativeFencePddV1::create_input(
            HELIOS_PACKAGE_GENERATION,
            HELIOS_NATIVE_FENCE_TYPE_DEFAULT,
            HELIOS_HNF1_FLAG_SHARED,
        );
        assert_eq!(
            input.validate_create_input(
                HELIOS_PACKAGE_GENERATION,
                0x1234,
                HELIOS_NATIVE_FENCE_TYPE_DEFAULT,
            ),
            Ok(())
        );
        assert_eq!(
            input.validate_returned(
                HELIOS_PACKAGE_GENERATION,
                0x1234,
                HELIOS_NATIVE_FENCE_TYPE_DEFAULT,
                HELIOS_HNF1_FLAG_SHARED,
            ),
            Err(HeliosNativeFencePddReject::ObjectGenerationNotZero)
        );

        let mut returned = input;
        returned.object_generation = 7;
        returned.adapter_luid = 0x1234;
        assert_eq!(
            returned.validate_returned(
                HELIOS_PACKAGE_GENERATION,
                0x1234,
                HELIOS_NATIVE_FENCE_TYPE_DEFAULT,
                HELIOS_HNF1_FLAG_SHARED,
            ),
            Ok(())
        );
        assert_eq!(
            HeliosNativeFencePddV1::from_bytes(&returned.to_bytes()),
            returned
        );
    }
}
