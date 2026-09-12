//! Virtual-monitor EDID 1.4. No allocation, runtime globals, or WDK dependency.
//! The 128-byte base block can represent only 12-bit extents and 16-bit clocks.
//! Return None rather than wrapping/truncating an unsupported host mode.
//! Wire contract: VESA E-EDID A2, sections 3.4, 3.6, 3.10.

pub fn build_edid(
    w: u32,
    h: u32,
    name: &str,
    publisher: &str,
    model_year: u16,
) -> Option<[u8; 128]> {
    if !(1..=4095).contains(&w)
        || !(1..=4095).contains(&h)
        || !(1990..=2245).contains(&model_year)
        || [name, publisher].iter().any(|text| {
            text.is_empty() || text.len() > 12 || !text.bytes().all(|b| (32..=126).contains(&b))
        })
    {
        return None;
    }
    let hb: u32 = ((w / 4) & !7).max(160);
    let vb: u32 = 45;
    let ht = w + hb;
    let vt = h + vb;
    let pc = (ht as u64 * vt as u64 * 60) / 10_000;
    if !(1..=65535).contains(&pc) {
        return None;
    }
    let pc = pc as u32;
    let mut e = [0u8; 128];
    // Header + manufacturer "HLS" (5-bit letters, A=1) + product 0x0001.
    e[0..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
    let mfg: u16 = (8 << 10) | (12 << 5) | 19;
    e[8] = (mfg >> 8) as u8;
    e[9] = (mfg & 0xFF) as u8;
    e[10] = 0x01;
    e[16] = 0xFF; // Model year, not a fabricated manufacture week.
    e[17] = (model_year - 1990) as u8;
    e[18] = 1; // EDID 1.4
    e[19] = 4;
    e[20] = 0xA0; // Digital, 8bpc; no physical connector standard is claimed.
                  // The virtual display has no physical dimensions. Encode its aspect ratio
                  // when EDID can represent it; 0/0 means unspecified for extreme ratios.
    let ratio = if w >= h {
        (w * 100 + h / 2) / h
    } else {
        (h * 100 + w / 2) / w
    };
    if (100..=354).contains(&ratio) {
        e[if w >= h { 21 } else { 22 }] = (ratio - 99) as u8;
    }
    e[23] = 120; // gamma 2.2
    e[24] = 0x06; // sRGB, preferred native timing, no continuous-frequency claim
    e[25..35].copy_from_slice(&[0xEE, 0x91, 0xA3, 0x54, 0x4C, 0x99, 0x26, 0x0F, 0x50, 0x54]);
    // Standard timings unused (0x0101 × 8).
    for b in e.iter_mut().take(54).skip(38) {
        *b = 0x01;
    }
    // Detailed Timing Descriptor 1 (bytes 54..72): w × h, ~60 Hz.
    let hfp = (hb / 3).min(88);
    let hsw = (hb / 5).min(44);
    let (vfp, vsw) = (3u32, 5u32);
    let d = &mut e[54..72];
    d[0] = (pc & 0xFF) as u8;
    d[1] = (pc >> 8) as u8;
    d[2] = (w & 0xFF) as u8;
    d[3] = (hb & 0xFF) as u8;
    d[4] = ((((w >> 8) & 0xF) << 4) | ((hb >> 8) & 0xF)) as u8;
    d[5] = (h & 0xFF) as u8;
    d[6] = (vb & 0xFF) as u8;
    d[7] = ((((h >> 8) & 0xF) << 4) | ((vb >> 8) & 0xF)) as u8;
    d[8] = (hfp & 0xFF) as u8;
    d[9] = (hsw & 0xFF) as u8;
    d[10] = (((vfp & 0xF) << 4) | (vsw & 0xF)) as u8;
    d[17] = 0x1E; // digital separate sync, +H +V
                  // No synthetic range-limit descriptor: this virtual display advertises its
                  // explicit timing only (E-EDID 1.4 section 3.10.3.3 makes ranges optional).
                  // ASCII text identifies the software publisher; HLS remains a stable PnP ID.
    text_descriptor(&mut e[72..90], 0xFE, publisher);
    // Descriptor 3: shared product name (0xFC). The build validates the EDID limit.
    text_descriptor(&mut e[90..108], 0xFC, name);
    // Descriptor 4: unused (0x10).
    e[108..113].copy_from_slice(&[0x00, 0x00, 0x00, 0x10, 0x00]);
    // Byte 126 = extension count (0). Byte 127 = checksum: sum of all 128 == 0 mod 256.
    let sum: u32 = e[..127].iter().map(|&b| b as u32).sum();
    e[127] = ((256 - (sum % 256)) % 256) as u8;
    Some(e)
}

fn text_descriptor(bytes: &mut [u8], tag: u8, text: &str) {
    bytes[3] = tag;
    bytes[5..].fill(b' ');
    bytes[5..5 + text.len()].copy_from_slice(text.as_bytes());
    bytes[5 + text.len()] = b'\n';
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_roundtrips_without_inventing_a_panel_or_frequency_range() {
        for (w, h) in [
            (320, 240),
            (1896, 1066),
            (1920, 1080),
            (3840, 2160),
            (1080, 1920),
        ] {
            let e = build_edid(w, h, "Helios vGPU", "WinBoat", 2026).unwrap();
            assert_eq!(e.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)), 0);
            assert_eq!(u32::from(e[56]) | (u32::from(e[58] >> 4) << 8), w);
            assert_eq!(u32::from(e[59]) | (u32::from(e[61] >> 4) << 8), h);
            assert_eq!(&e[12..16], &[0; 4]); // No fabricated serial number.
            assert_eq!(&e[16..20], &[255, 36, 1, 4]); // Model year 2026, EDID 1.4.
            assert!(e[21] == 0 || e[22] == 0); // Aspect ratio, never centimeters.
            assert_eq!(&e[77..85], b"WinBoat\n");
            assert_eq!(&e[95..107], b"Helios vGPU\n");
            assert_eq!(e[75], 0xFE); // Publisher text replaces the false ranges.
            let ht = w + (((w / 4) & !7).max(160));
            let vt = h + 45;
            let clock = u32::from(u16::from_le_bytes([e[54], e[55]])) * 10_000;
            assert!(clock <= ht * vt * 60 && ht * vt * 60 - clock < 10_000);
        }
    }

    #[test]
    fn invalid_extents_and_identity_are_refused() {
        for (w, h) in [
            (0, 1080),
            (1920, 0),
            (4096, 2160),
            (4095, 4095),
            (u32::MAX, u32::MAX),
        ] {
            assert!(build_edid(w, h, "Helios vGPU", "WinBoat", 2026).is_none());
        }
        for name in ["", "Too long a product", "Bad\nName", "Non-ASCII é"] {
            assert!(build_edid(1920, 1080, name, "WinBoat", 2026).is_none());
        }
        assert!(build_edid(1920, 1080, "Helios vGPU", "Publisher too long", 2026).is_none());
        assert!(build_edid(1920, 1080, "Helios vGPU", "WinBoat", 1989).is_none());
        assert!(build_edid(1920, 1080, "Helios vGPU", "WinBoat", 2246).is_none());
    }
}
