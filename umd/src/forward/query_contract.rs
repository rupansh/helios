//! Platform-neutral query translation and publication rules.
//! The WDK/API constants and layouts are checked against this table in queries.rs.
//! Run without a Windows linker: rustc --test query_contract.rs -o <test-path>.

use core::ffi::c_void;
use core::mem::MaybeUninit;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct QuerySpec {
    pub api_query: i32,
    pub ddi_size: u32,
    pub api_size: u32,
}

pub(crate) const fn query_spec(ddi_query: i32) -> Option<QuerySpec> {
    // DDI 4 is the legacy eight-counter pipeline query; DDI 8 is the
    // eleven-counter query. API stream statistics and overflow are interleaved.
    let (api_query, ddi_size, api_size) = match ddi_query {
        0 => (0, 4, 4),   // EVENT
        1 => (1, 8, 8),   // OCCLUSION
        2 => (2, 8, 8),   // TIMESTAMP
        3 => (3, 16, 16), // TIMESTAMP_DISJOINT
        4 => (4, 64, 88), // D3D10 PIPELINE_STATISTICS
        5 => (5, 4, 4),   // OCCLUSION_PREDICATE
        6 => (6, 16, 16), // SO_STATISTICS
        7 => (7, 4, 4),   // SO_OVERFLOW_PREDICATE
        8 => (4, 88, 88), // D3D11 PIPELINE_STATISTICS
        9 => (8, 16, 16), // SO_STATISTICS_STREAM0
        10 => (10, 16, 16),
        11 => (12, 16, 16),
        12 => (14, 16, 16),
        13 => (9, 4, 4), // SO_OVERFLOW_PREDICATE_STREAM0
        14 => (11, 4, 4),
        15 => (13, 4, 4),
        16 => (15, 4, 4),
        _ => return None,
    };
    Some(QuerySpec {
        api_query,
        ddi_size,
        api_size,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum QueryError {
    InvalidRequest,
    Pending,
    Backend(i32),
}

/// Largest API result, with alignment sufficient for every API query structure
/// on both Windows architectures. Zeroing also makes disjoint-query padding
/// deterministic instead of copying uninitialized bytes to the runtime.
#[repr(C, align(8))]
pub(crate) struct ApiResult([u8; 88]);

impl ApiResult {
    pub fn as_mut_ptr(&mut self) -> *mut c_void {
        self.0.as_mut_ptr().cast()
    }
}

/// `None` is a status-only poll, including a non-null caller pointer with size
/// zero. The engine never sees the caller's buffer: even EVENT can write FALSE
/// before returning S_FALSE. Only S_OK publishes bytes. The legacy pipeline
/// result is the eight-field prefix of the eleven-field API result, with every
/// field offset compile-checked in queries.rs.
pub(crate) fn get_data(
    spec: QuerySpec,
    output: Option<&mut [MaybeUninit<u8>]>,
    flags: u32,
    backend: impl FnOnce(Option<&mut ApiResult>, u32, u32) -> i32,
) -> Result<(), QueryError> {
    if flags & !1 != 0
        || spec.api_size > 88
        || spec.ddi_size > spec.api_size
        || output
            .as_ref()
            .is_some_and(|bytes| bytes.len() != spec.ddi_size as usize)
    {
        return Err(QueryError::InvalidRequest);
    }
    let mut result = ApiResult([0; 88]);
    let hr = if output.is_some() {
        backend(Some(&mut result), spec.api_size, flags)
    } else {
        backend(None, 0, flags)
    };
    match hr {
        0 => {
            if let Some(bytes) = output {
                for (destination, source) in bytes.iter_mut().zip(&result.0) {
                    destination.write(*source);
                }
            }
            Ok(())
        }
        1 => Err(QueryError::Pending),
        hr => Err(QueryError::Backend(hr)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slots(bytes: &mut [u8]) -> &mut [MaybeUninit<u8>] {
        // SAFETY: MaybeUninit<u8> has the same size/alignment as u8, accepts
        // every initialized byte, and this view retains the exclusive borrow.
        unsafe { core::slice::from_raw_parts_mut(bytes.as_mut_ptr().cast(), bytes.len()) }
    }

    #[test]
    fn caller_storage_may_be_uninitialized_on_pending_or_success() {
        let spec = query_spec(8).unwrap();
        let mut bytes = [MaybeUninit::<u8>::uninit(); 88];
        assert_eq!(
            get_data(spec, Some(&mut bytes), 1, |_, _, _| 1),
            Err(QueryError::Pending)
        );
        get_data(spec, Some(&mut bytes), 0, |data, _, _| {
            data.unwrap().0.fill(0x37);
            0
        })
        .unwrap();
        for byte in bytes {
            // SAFETY: successful publication initialized every output byte.
            assert_eq!(unsafe { byte.assume_init() }, 0x37);
        }
    }

    #[test]
    fn every_ddi_type_selects_the_correct_api_and_result_size() {
        let expected = [
            (0, 4, 4),
            (1, 8, 8),
            (2, 8, 8),
            (3, 16, 16),
            (4, 64, 88),
            (5, 4, 4),
            (6, 16, 16),
            (7, 4, 4),
            (4, 88, 88),
            (8, 16, 16),
            (10, 16, 16),
            (12, 16, 16),
            (14, 16, 16),
            (9, 4, 4),
            (11, 4, 4),
            (13, 4, 4),
            (15, 4, 4),
        ];
        for (ddi, &(api, size, backend_size)) in expected.iter().enumerate() {
            let spec = query_spec(ddi as i32).unwrap();
            assert_eq!(
                (spec.api_query, spec.ddi_size, spec.api_size),
                (api, size, backend_size)
            );
        }
        for invalid in [-1, 17, 4096, 0x40000000, i32::MAX] {
            assert_eq!(query_spec(invalid), None);
        }
    }

    #[test]
    fn pipeline_stats_uses_88_backend_bytes_and_preserves_legacy_canaries() {
        for (ddi, size) in [(4, 64), (8, 88)] {
            let mut guarded = [0xa5; 90];
            get_data(
                query_spec(ddi).unwrap(),
                Some(slots(&mut guarded[1..1 + size])),
                0,
                |data, n, flags| {
                    assert_eq!((n, flags), (88, 0));
                    let bytes = &mut data.unwrap().0;
                    for (i, word) in bytes.chunks_exact_mut(8).enumerate() {
                        word.copy_from_slice(&(0x1020304050607000_u64 + i as u64).to_ne_bytes());
                    }
                    0
                },
            )
            .unwrap();
            for (i, word) in guarded[1..1 + size].chunks_exact(8).enumerate() {
                assert_eq!(word, (0x1020304050607000_u64 + i as u64).to_ne_bytes());
            }
            assert_eq!(guarded[0], 0xa5);
            assert!(guarded[1 + size..].iter().all(|&b| b == 0xa5));
        }
    }

    #[test]
    fn pending_and_errors_do_not_publish_even_if_backend_writes() {
        for ddi in 0..=16 {
            let spec = query_spec(ddi).unwrap();
            for hr in [1, -1, 0x887a0005_u32 as i32, 2] {
                let mut output = vec![0xa5; spec.ddi_size as usize];
                let result = get_data(spec, Some(slots(&mut output)), 1, |data, _, flags| {
                    assert_eq!(flags, 1);
                    data.unwrap().0.fill(0);
                    hr
                });
                assert_eq!(
                    result,
                    Err(if hr == 1 {
                        QueryError::Pending
                    } else {
                        QueryError::Backend(hr)
                    })
                );
                assert!(output.iter().all(|&b| b == 0xa5));
            }
        }
    }

    #[test]
    fn polling_passes_null_zero_and_preserves_flush_flags() {
        for ddi in 0..=16 {
            for flags in [0, 1] {
                let result = get_data(
                    query_spec(ddi).unwrap(),
                    None,
                    flags,
                    |data, size, actual_flags| {
                        assert!(data.is_none());
                        assert_eq!((size, actual_flags), (0, flags));
                        1
                    },
                );
                assert_eq!(result, Err(QueryError::Pending));
            }
        }
    }

    #[test]
    fn invalid_sizes_or_flags_never_reach_backend_or_change_output() {
        let spec = query_spec(8).unwrap();
        for size in [0, 4, 16, 64, 87, 89] {
            let mut output = vec![0xa5; size];
            assert_eq!(
                get_data(spec, Some(slots(&mut output)), 0, |_, _, _| panic!(
                    "backend called"
                )),
                Err(QueryError::InvalidRequest)
            );
            assert!(output.iter().all(|&b| b == 0xa5));
        }
        assert_eq!(
            get_data(spec, None, 2, |_, _, _| panic!("backend called")),
            Err(QueryError::InvalidRequest)
        );
    }

    #[test]
    fn all_query_results_are_aligned_and_only_their_size_is_published() {
        for ddi in 0..=16 {
            let spec = query_spec(ddi).unwrap();
            let mut guarded = [0xa5; 90];
            let size = spec.ddi_size as usize;
            get_data(
                spec,
                Some(slots(&mut guarded[1..1 + size])),
                0,
                |data, backend_size, _| {
                    let data = data.unwrap();
                    assert_eq!(data.as_mut_ptr() as usize % 8, 0);
                    assert_eq!(backend_size, spec.api_size);
                    data.0[..backend_size as usize].fill(0x37);
                    0
                },
            )
            .unwrap();
            assert!(guarded[1..1 + size].iter().all(|&b| b == 0x37));
            assert_eq!(guarded[0], 0xa5);
            assert!(guarded[1 + size..].iter().all(|&b| b == 0xa5));
        }
    }
}
