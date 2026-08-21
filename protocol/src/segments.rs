//! WDDM segment vocabulary shared by the native-render contract and KMD.
//!
//! The active package exposes one allocation-placement segment to HVM1/HOC1:
//! the ordinary WDDM aperture. Local VidMm capacity is a KMD-private adapter
//! fact and is not an HVM1 placement or a host protocol.

/// Segment 1 — the ordinary WDDM aperture used by K2a shared backing.
pub const HELIOS_SEGMENT_ID_APERTURE: u32 = 1;
