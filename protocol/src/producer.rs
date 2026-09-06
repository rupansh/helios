//! Allocation producer status and the version-one control ABI. Producer-ready
//! never authorizes consumer release, allocation reuse or external ownership.
use crate::HeliosEscapeHeader;
use bytemuck::{Pod, Zeroable};

pub const HELIOS_ESCAPE_PRODUCER: u32 = 0x0013;
pub const HELIOS_PRODUCER_VERSION: u32 = 1;
pub const HELIOS_PRODUCER_SLOTS: usize = 8192;
pub const HELIOS_PRODUCER_MAP: u32 = 1;
pub const HELIOS_PRODUCER_BIND: u32 = 2;
pub const HELIOS_PRODUCER_PUBLISH: u32 = 3;
pub const HELIOS_PRODUCER_WAIT: u32 = 4;
pub const HELIOS_PRODUCER_CANCEL: u32 = 5;
pub const HELIOS_PRODUCER_RELEASE: u32 = 6;
pub const HELIOS_PRODUCER_ABORT: u32 = 7;
pub const HELIOS_PRODUCER_READY: u32 = 0;
pub const HELIOS_PRODUCER_PENDING: u32 = 1;
pub const HELIOS_PRODUCER_TERMINAL: u32 = 2;

/// All control operations are device-scoped. MAP returns a status view only;
/// only BIND (after dxgkrnl allocation resolution) grants control authority.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct HeliosEscapeProducer {
    pub header: HeliosEscapeHeader,
    pub version: u32,
    pub op: u32,
    pub binding: u64,
    pub generation: u64,
    pub epoch: u64,
    pub stream_cookie: u64,
    pub event: u64,
    pub user_va: u64,
    pub allocation: u32,
    pub ctx_id: u32,
    pub value: u32,
    pub slot: u32,
    pub state: u32,
    pub size: u32,
}

/// Cache-line-sized direct view. KMD writes a seqlock snapshot using aligned
/// atomics. Readers retry at most eight times and report contention explicitly.
#[repr(C, align(64))]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct HeliosProducerStatus {
    pub sequence: u64,
    pub generation: u64,
    pub announced: u64,
    pub completed: u64,
    pub status: u32,
    pub reserved: [u32; 7],
}

const _: () = {
    assert!(core::mem::size_of::<HeliosEscapeProducer>() == 96);
    assert!(core::mem::offset_of!(HeliosEscapeProducer, binding) == 24);
    assert!(core::mem::size_of::<HeliosProducerStatus>() == 64);
    assert!(core::mem::offset_of!(HeliosProducerStatus, status) == 32);
};
