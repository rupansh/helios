//! Host-testable admission rule for `DXGKQAITYPE_UMDRIVERPRIVATE`.
//!
//! `D3DDDICB_QUERYADAPTERINFO` gives a UMD one private buffer.  For this
//! callback dxgkrnl presents that buffer to the KMD as `pOutputData`; it does
//! not create a private input channel.  Keep that measured WDDM shape in this
//! dependency-free leaf so the kernel callback and its host regression share
//! one rule.

use helios_protocol::HELIOS_UMD_ADAPTER_INFO_BYTES;

/// Accept only the exact output-only buffer used by the package UMDs.
///
/// `hKmdProcessHandle` is deliberately absent.  The WDK permits it to be null,
/// and OpenAdapter reaches this query before a D3D device/process object exists.
pub const fn is_exact(
    input_size: u32,
    input_is_null: bool,
    output_size: u32,
    output_is_null: bool,
    output_is_aligned: bool,
) -> bool {
    input_size == 0
        && input_is_null
        && output_size == HELIOS_UMD_ADAPTER_INFO_BYTES
        && !output_is_null
        && output_is_aligned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admits_the_measured_output_only_open_adapter_shape() {
        assert!(is_exact(0, true, 48, false, true));
    }

    #[test]
    fn rejects_every_single_field_shape_mutation() {
        let cases = [
            (1, true, 48, false, true),
            (0, false, 48, false, true),
            (0, true, 47, false, true),
            (0, true, 49, false, true),
            (0, true, 48, true, true),
            (0, true, 48, false, false),
        ];
        for (input_size, input_is_null, output_size, output_is_null, output_is_aligned) in cases {
            assert!(!is_exact(
                input_size,
                input_is_null,
                output_size,
                output_is_null,
                output_is_aligned,
            ));
        }
    }
}
