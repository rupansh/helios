//! The counting noop-DDI idiom, and the size-derived table stubber.
//!
//! Moved from `umd/src/device_funcs.rs:640-754, 1128-1145` (`DECISIONS.md` D3b,
//! stage S2).
//!
//! ⛔ **`ddi_calc_size`'s 256-byte answer did NOT move.** D3b names it: that is
//! a D3D11 claim about `CalcPrivate*Size` stubs, not a shared fact, and the
//! D3D12 private-data sizes are a different contract. It stays in `umd`.
//!
//! # What is shared, and why it is worth sharing
//!
//! A WDDM UMD is handed a table of function pointers and must fill **every**
//! slot before returning, or the runtime calls through an uninitialised one.
//! Both drivers therefore need signature-preserving stubs, counters and a
//! one-shot backtrace so an unexpected hit names its caller. Table field lists
//! live with the generated WDK types in each driver.

// The signature, including argument widths and return shape, comes from the
// target's WDK field type. This is essential on x86: APIENTRY is stdcall and
// the callee must pop exactly its declared arguments.

/// The action performed by an unimplemented DDI, with its exact return type.
/// Reporters own the counter and explicit fallback policy; the shared adapter
/// owns only the calling convention and argument signature.
pub trait StubReport<Return> {
    fn hit() -> Return;
}

/// A function-pointer field that can receive a signature-preserving stub.
pub trait TypedStub<Report>: Sized {
    fn typed_stub() -> Self;
}

/// Install a stub inferred from the WDK-generated field type, without a cast.
pub fn install_stub<Report, Field: TypedStub<Report>>(field: &mut Field) {
    *field = Field::typed_stub();
}

macro_rules! typed_stub {
    ($($arg:ident),* $(,)?) => {
        impl<Report, Return, $($arg,)*> TypedStub<Report>
            for Option<unsafe extern "system" fn($($arg),*) -> Return>
        where
            Report: StubReport<Return>,
        {
            fn typed_stub() -> Self {
                unsafe extern "system" fn invoke<Report, Return, $($arg,)*>(
                    $(_: $arg),*
                ) -> Return
                where
                    Report: StubReport<Return>,
                {
                    Report::hit()
                }
                Some(invoke::<Report, Return, $($arg,)*>)
            }
        }
    };
}

typed_stub!();
typed_stub!(A);
typed_stub!(A, B);
typed_stub!(A, B, C);
typed_stub!(A, B, C, D);
typed_stub!(A, B, C, D, E);
typed_stub!(A, B, C, D, E, F);
typed_stub!(A, B, C, D, E, F, G);
typed_stub!(A, B, C, D, E, F, G, H);
typed_stub!(A, B, C, D, E, F, G, H, I);
typed_stub!(A, B, C, D, E, F, G, H, I, J);
typed_stub!(A, B, C, D, E, F, G, H, I, J, K);
typed_stub!(A, B, C, D, E, F, G, H, I, J, K, L);
typed_stub!(A, B, C, D, E, F, G, H, I, J, K, L, M);
typed_stub!(A, B, C, D, E, F, G, H, I, J, K, L, M, N);
typed_stub!(A, B, C, D, E, F, G, H, I, J, K, L, M, N, O);
typed_stub!(A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P);

#[cfg(windows)]
use core::ffi::c_void;

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn RtlCaptureStackBackTrace(
        frames_to_skip: u32,
        frames_to_capture: u32,
        back_trace: *mut *mut c_void,
        back_trace_hash: *mut u32,
    ) -> u16;
}

/// Log up to 32 return addresses, tagged.
///
/// The first hit of a noop stub is the interesting one: it says which runtime
/// call reached a slot the driver never implemented, which is the difference
/// between "this DDI is missing" and "this DDI is missing *and something
/// actually calls it*". Subsequent hits log only a count — capturing and
/// formatting 32 frames on every hit was itself a measured cost.
///
/// # Safety
/// Calls `RtlCaptureStackBackTrace`, which walks the caller's stack. Safe in
/// any ordinary user-mode context; not callable from a context where the stack
/// is being unwound.
#[cfg(windows)]
pub unsafe fn log_backtrace(tag: &str) {
    let mut frames = [core::ptr::null_mut::<c_void>(); 32];
    // SAFETY: the caller promises an ordinary user-mode stack outside unwind;
    // the frame array provides frames.len() writable pointer slots.
    let captured = unsafe {
        RtlCaptureStackBackTrace(
            0,
            frames.len() as u32,
            frames.as_mut_ptr(),
            core::ptr::null_mut(),
        )
    };
    let mut out = String::new();
    for (i, frame) in frames.iter().take(captured as usize).enumerate() {
        out.push_str(&format!(" #{i}=0x{:x}", *frame as usize));
    }
    crate::log_error!("{tag} stack{out}");
}

#[cfg(test)]
mod tests {
    use super::{install_stub, StubReport};
    use core::sync::atomic::{AtomicUsize, Ordering};

    static HITS: AtomicUsize = AtomicUsize::new(0);
    struct Refuse;
    impl StubReport<i32> for Refuse {
        fn hit() -> i32 {
            HITS.fetch_add(1, Ordering::Relaxed);
            0x80004001u32 as i32
        }
    }

    #[repr(C)]
    #[derive(Debug, PartialEq)]
    struct Pair {
        first: u64,
        second: u32,
    }
    struct StructResult;
    impl StubReport<Pair> for StructResult {
        fn hit() -> Pair {
            Pair {
                first: 0x123456789abcdef0,
                second: 17,
            }
        }
    }

    #[test]
    fn preserves_mixed_width_arguments_and_hresult() {
        let mut slot: Option<unsafe extern "system" fn(u32, u64, *const u8, f32, usize) -> i32> =
            None;
        install_stub::<Refuse, _>(&mut slot);
        // SAFETY: install_stub initializes the exact signature; no argument is dereferenced.
        let result = unsafe { slot.unwrap()(3, u64::MAX, core::ptr::null(), 1.5, 7) };
        assert_eq!(result, 0x80004001u32 as i32);
        assert_eq!(HITS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn preserves_struct_return_abi() {
        let mut slot: Option<unsafe extern "system" fn(u32, u64) -> Pair> = None;
        install_stub::<StructResult, _>(&mut slot);
        // SAFETY: install_stub initializes the exact signature and does not use its arguments.
        let result = unsafe { slot.unwrap()(5, u64::MAX) };
        assert_eq!(
            result,
            Pair {
                first: 0x123456789abcdef0,
                second: 17
            }
        );
    }
}
