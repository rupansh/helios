//! KMD custody for the pure control-ownership transport generation.
//! This foundation binds identity to a minted domain, never to an adapter address.
//! The stored address is a one-shot construction guard, grants no authority, and a future sole generation gateway must validate it exactly.

use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use helios_kmd_logic::control_ownership::{
    TransportDomainRoot, TransportGeneration as OwnershipGeneration,
};

use crate::adapter::AdapterContext;
use crate::sync::SpinLock;

static TRANSPORT_DOMAIN_HIGH_WATER: AtomicU64 = AtomicU64::new(0);
pub(crate) static TRANSPORT_DOMAIN_EXHAUSTED: AtomicU32 = AtomicU32::new(0);
static TRANSPORT_OWNER_REBIND_REFUSED: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TransportDomainExhausted;

struct OwnerState {
    construction_address: usize,
    _generation: OwnershipGeneration,
}

pub(crate) struct TransportOwner {
    state: SpinLock<OwnerState>,
}

impl TransportOwner {
    /// PASSIVE_LEVEL: exhaustion publishes its fixed diagnostic breadcrumbs.
    pub(crate) fn unbound() -> Result<Self, TransportDomainExhausted> {
        let root = mint_domain_root()?;
        Ok(Self {
            state: SpinLock::new(OwnerState {
                construction_address: 0,
                _generation: OwnershipGeneration::bootstrap(root),
            }),
        })
    }

    /// Safety: at PASSIVE_LEVEL, `adapter` is the owner's final construction
    /// address, recorded exactly once before publication.
    pub(crate) unsafe fn bind_adapter_once(&self, adapter: NonNull<AdapterContext>) {
        let refused = {
            let mut state = self.state.lock();
            if state.construction_address != 0 {
                true
            } else {
                state.construction_address = adapter.as_ptr() as usize;
                false
            }
        };
        if refused {
            let count = TRANSPORT_OWNER_REBIND_REFUSED
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_add(1);
            crate::diag::record_named_bytes(b"CtDomReb", count);
            crate::diag::record(0x0A00_00E5);
        }
    }
}

fn mint_domain_root() -> Result<TransportDomainRoot, TransportDomainExhausted> {
    let previous = TRANSPORT_DOMAIN_HIGH_WATER
        .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| record_domain_exhaustion())?;
    let Some(issued) = previous.checked_add(1) else {
        return Err(record_domain_exhaustion());
    };

    // SAFETY: the non-wrapping driver-load high-water issues each nonzero value once.
    unsafe { TransportDomainRoot::new(issued) }.map_err(|_| record_domain_exhaustion())
}

fn record_domain_exhaustion() -> TransportDomainExhausted {
    let count = TRANSPORT_DOMAIN_EXHAUSTED
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1);
    crate::diag::record_named_bytes(b"CtDomExh", count);
    crate::diag::record(0x0A00_00E4);
    TransportDomainExhausted
}
