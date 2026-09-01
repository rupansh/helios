//! The 16-byte header `DxgkDdiPresent` writes at offset 0 of a Present
//! packet's KMD-only DMA private data, so `DxgkDdiSubmitCommand` can tell it
//! from the 64-byte HOB1 render record that lives at the same offset on the
//! same outer context. ROADMAP D5 (2026-09-01): without it a windowed app's
//! 4-byte BLT present packet was parsed as HOB1, refused as
//! `ResubmissionMismatch`, and the refusal poisoned the adapter's K9 engine.

/// `"HPDP"` little-endian. Distinct from HOB1 `"HKD1"`, HOS1 `"HOS1"` and the
/// flip record `"HPFL"` at offset 32.
pub const PRESENT_DMA_HEADER_MAGIC: u32 = 0x5044_5048;
pub const PRESENT_DMA_HEADER_VERSION: u32 = 1;
pub const PRESENT_DMA_HEADER_BYTES: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum PresentDmaKind {
    /// `DXGK_PRESENTFLAGS.Blt`: the UMD copied before `pfnPresentCb`; the
    /// packet is an ordering marker only.
    Blt = 1,
    /// DMA-buffer flip: a `PresentFlipPrivate` record follows at offset 32 and
    /// must be armed at submit.
    Flip = 2,
    /// Multiplane-overlay present carried in a DMA buffer.
    Mpo = 3,
    /// Any other `DXGKARG_PRESENT` shape that still emitted a packet.
    Other = 4,
}

impl PresentDmaKind {
    pub const fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            1 => Some(Self::Blt),
            2 => Some(Self::Flip),
            3 => Some(Self::Mpo),
            4 => Some(Self::Other),
            _ => None,
        }
    }
}

pub fn encode(kind: PresentDmaKind) -> [u8; PRESENT_DMA_HEADER_BYTES] {
    let mut bytes = [0u8; PRESENT_DMA_HEADER_BYTES];
    bytes[0..4].copy_from_slice(&PRESENT_DMA_HEADER_MAGIC.to_le_bytes());
    bytes[4..8].copy_from_slice(&PRESENT_DMA_HEADER_VERSION.to_le_bytes());
    bytes[8..12].copy_from_slice(&(kind as u32).to_le_bytes());
    bytes
}

/// `None` for anything that is not exactly a version-1 header with a known
/// kind and a zero reserved word — an HOB1 record, a zeroed buffer, a short
/// window.
pub fn decode(bytes: &[u8]) -> Option<PresentDmaKind> {
    if bytes.len() < PRESENT_DMA_HEADER_BYTES {
        return None;
    }
    let word =
        |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    if word(0) != PRESENT_DMA_HEADER_MAGIC || word(4) != PRESENT_DMA_HEADER_VERSION || word(12) != 0
    {
        return None;
    }
    PresentDmaKind::from_raw(word(8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_round_trips() {
        for kind in [
            PresentDmaKind::Blt,
            PresentDmaKind::Flip,
            PresentDmaKind::Mpo,
            PresentDmaKind::Other,
        ] {
            assert_eq!(decode(&encode(kind)), Some(kind));
        }
    }

    #[test]
    fn decode_accepts_a_longer_window_and_ignores_the_tail() {
        let mut buf = [0xA5u8; 64];
        buf[..16].copy_from_slice(&encode(PresentDmaKind::Blt));
        assert_eq!(decode(&buf), Some(PresentDmaKind::Blt));
    }

    #[test]
    fn foreign_records_and_garbage_are_not_present_packets() {
        // HOB1 ("HKD1") and HOS1 ("HOS1") magics at offset 0, as a render
        // record on the same context would carry.
        for magic in [0x3144_4B48u32, 0x3153_4F48u32, 0, 0xFFFF_FFFF] {
            let mut buf = encode(PresentDmaKind::Blt);
            buf[0..4].copy_from_slice(&magic.to_le_bytes());
            assert_eq!(decode(&buf), None, "magic {magic:#x}");
        }
        assert_eq!(decode(&[0u8; 64]), None);
        assert_eq!(decode(&[0xFFu8; 64]), None);
    }

    #[test]
    fn version_kind_and_reserved_are_all_checked() {
        let mut bad_version = encode(PresentDmaKind::Flip);
        bad_version[4..8].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(decode(&bad_version), None);

        let mut bad_kind = encode(PresentDmaKind::Flip);
        bad_kind[8..12].copy_from_slice(&5u32.to_le_bytes());
        assert_eq!(decode(&bad_kind), None);
        let mut zero_kind = encode(PresentDmaKind::Flip);
        zero_kind[8..12].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(decode(&zero_kind), None);

        let mut reserved = encode(PresentDmaKind::Flip);
        reserved[12] = 1;
        assert_eq!(decode(&reserved), None);
    }

    #[test]
    fn short_windows_are_refused() {
        let full = encode(PresentDmaKind::Mpo);
        assert_eq!(decode(&full[..15]), None);
        assert_eq!(decode(&[]), None);
    }

    #[test]
    fn header_fits_below_the_flip_record() {
        // `PresentFlipPrivate` is written at offset 32 of the same buffer.
        assert!(PRESENT_DMA_HEADER_BYTES <= 32);
    }
}
