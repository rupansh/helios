//! Ordered snapshots for direct scan-out and WindowedBlt presentation.
//!
//! DXVK copies/resolves the presented source before the frame submission gate.
//! The resulting private descriptor carries the snapshot's exact resource ID
//! and layout through the KMD. MSAA and sRGB normalization is mandatory;
//! failure to produce that representation refuses Present.
//!
//! Rings are keyed by geometry, source format, and presentation purpose. Direct
//! scan-out rings remain alive until device teardown. A WindowedBlt ring may
//! be reclaimed between serialized presents only after the KMD reports every
//! slot idle, including deferred GPU reads and CPU mirrors. Both paths share a
//! fixed retained-memory and ring-count budget.

use super::*;

use crate::device_funcs::{SnapshotRing, SnapshotRingCache, SnapshotSlot};

/// Ring depth. 4 matches the deepest DXGI flip ring the direct primary
/// rotates through, giving a reuse distance of ~4 present periods before a
/// slot is re-written — and the D4a acquire additionally gates the blit on
/// any still-in-flight host read of the slot (the designed backstop).
pub(crate) const SNAPSHOT_RING_SLOTS: usize = 4;

/// A browser can own several independently presented child surfaces. Eight
/// geometry keys covers that shape while keeping the cache's object count
/// strictly bounded. Idle WindowedBlt rings make room for new geometries.
const SNAPSHOT_CACHE_MAX_RINGS: usize = 8;
/// Retained dedicated backing across all rings. At 32bpp this admits one 4K
/// ring with ample tiling/alignment headroom, or many normal window surfaces,
/// without allowing resize churn to consume the device's full VRAM budget.
const SNAPSHOT_CACHE_MAX_BYTES: u64 = 256 * 1024 * 1024;

/// Everything `finish_present` needs to substitute the descriptor: S_i's
/// identity and layout, captured from the ring slot the blit just targeted.
/// Constructed in exactly one place, the successful-blit arm of
/// [`snapshot_for_present`].
pub(crate) struct SnapshotPlan {
    pub(crate) resid: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pitch: u32,
    pub(crate) plane_offset: u64,
    /// Format the presented application resource had when the copy was
    /// recorded. Used only by the stale-private guard.
    pub(crate) source_dxgi_format: u32,
    /// Format of the converted snapshot resource published to the KMD.
    pub(crate) dxgi_format: u32,
    pub(crate) venus_alloc_size: u64,
    pub(crate) memory_type_index: u32,
    pub(crate) purpose: SnapshotPurpose,
}

/// The typed consumer of a snapshot.  The direct arm binds it; the windowed
/// BLT arm imports it as an ordinary Vulkan source.  Keeping the distinction in
/// the plan prevents a WindowedBlt descriptor from ever reaching a scanout
/// refresh arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SnapshotPurpose {
    DirectFlip,
    WindowedBlt,
}

/// Presents whose descriptor was substituted with a snapshot (the UMD-side
/// twin of the KMD's `SnSub`).
pub(crate) static SNAP_SUBSTITUTED: AtomicUsize = AtomicUsize::new(0);
/// Ring builds that failed (create/identity/pitch).
pub(crate) static SNAP_RING_CREATE_FAILS: AtomicUsize = AtomicUsize::new(0);
/// Blit bridge calls that failed or reported an extent mismatch.
pub(crate) static SNAP_COPY_FAILS: AtomicUsize = AtomicUsize::new(0);
/// New geometry keys refused at the hard count/byte budget.
pub(crate) static SNAP_CACHE_REFUSALS: AtomicUsize = AtomicUsize::new(0);
static SNAP_RING_RECLAIMS: AtomicUsize = AtomicUsize::new(0);
/// `apply_snapshot_override` refusals: the private data re-read in
/// `finish_present` was absent or no longer matched the geometry the blit
/// was recorded against. The stale-descriptor guard's loud half.
pub(crate) static SNAP_PRIVATE_SKIPS: AtomicUsize = AtomicUsize::new(0);
/// A source could not be normalized to the single-sample, encoded-byte
/// presentation/import contract. No incompatible allocation may be published.
pub(crate) static SNAP_REQUIRED_REFUSALS: AtomicUsize = AtomicUsize::new(0);

/// Whether the raw allocation violates the presentation consumer's contract.
///
/// # Safety
/// `src_h` is the live source handle of the current presentation DDI.
pub(crate) unsafe fn requires_present_snapshot(src_h: ddi::D3D10DDI_HRESOURCE) -> bool {
    // SAFETY: both helpers inspect the live resource specified by the caller.
    unsafe {
        resource_sample_count(src_h) > 1 || matches!(resource_dxgi_format(src_h).0, 29 | 91 | 93)
    }
}

pub(crate) fn required_snapshot_refused(reason: &str) -> i32 {
    let n = SNAP_REQUIRED_REFUSALS.fetch_add(1, Ordering::Relaxed);
    if n < 16 || n % 512 == 0 {
        log_error!(
            "Present refused: required sample/color normalization unavailable: {} (count={})",
            reason,
            n + 1
        );
    }
    E_FAIL
}

/// Select the format carried by a scan-out snapshot.
///
/// The KMD/QEMU direct-primary contract consumes ordinary 32-bit AR24/XR24
/// pixels. Packed R10G10B10A2 is also four bytes per texel, but publishing its
/// words as AR24 reinterprets bit fields as byte channels. Keep format 24 out
/// of the scan-out allocator and convert it numerically into RGBA8 instead.
fn snapshot_scanout_format(source_dxgi_format: u32) -> Option<u32> {
    match source_dxgi_format {
        24 => Some(28),
        // Present preserves sRGB encoded bytes in the corresponding UNORM
        // scanout format. The bridge resolves MSAA in the source format
        // before doing a compatible bit copy; no sRGB-to-linear conversion.
        29 => Some(28),
        91 => Some(87),
        93 => Some(88),
        28 | 87 | 88 => Some(source_dxgi_format),
        _ => None,
    }
}

fn counter_summary() -> String {
    format!(
        "sub={} ring_fails={} copy_fails={} cache_refusals={} ring_reclaims={} private_skips={} required_refusals={}",
        SNAP_SUBSTITUTED.load(Ordering::Relaxed),
        SNAP_RING_CREATE_FAILS.load(Ordering::Relaxed),
        SNAP_COPY_FAILS.load(Ordering::Relaxed),
        SNAP_CACHE_REFUSALS.load(Ordering::Relaxed),
        SNAP_RING_RECLAIMS.load(Ordering::Relaxed),
        SNAP_PRIVATE_SKIPS.load(Ordering::Relaxed),
        SNAP_REQUIRED_REFUSALS.load(Ordering::Relaxed),
    )
}

/// Build the 4-slot ring against the presented primary's geometry.
///
/// Any per-slot failure releases the unpublished slots and counts the refusal.
///
/// # Safety
/// `h` must be the live device the caller resolved `dev` from.
unsafe fn build_ring(
    dev: &HeliosDevice,
    h: Hdevice,
    width: u32,
    height: u32,
    source_dxgi_format: u32,
    scanout_dxgi_format: u32,
    purpose: SnapshotPurpose,
) -> Option<SnapshotRing> {
    // The composition-surface baseline. Sharing/export is driven by the
    // DirectOptimalScanout marker inside create_ddi_scanout_texture2d, not by
    // these flags, and DXVK images are transfer-dst capable regardless.
    let bind = (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32;
    let mut slots: Vec<SnapshotSlot> = Vec::with_capacity(SNAPSHOT_RING_SLOTS);
    for i in 0..SNAPSHOT_RING_SLOTS {
        let Some((res, row_pitch, plane_offset)) = dev.dxvk.create_scanout_texture2d(
            width,
            height,
            scanout_dxgi_format,
            bind,
            0,
            purpose == SnapshotPurpose::WindowedBlt,
        ) else {
            let n = SNAP_RING_CREATE_FAILS.fetch_add(1, Ordering::Relaxed);
            if n < 16 || n % 512 == 0 {
                log_error!(
                    "scanout-snapshot: ring slot {i} create FAILED {}x{} src_fmt={} scanout_fmt={} ({})",
                    width,
                    height,
                    source_dxgi_format,
                    scanout_dxgi_format,
                    counter_summary()
                );
            }
            return None;
        };
        // The KMD binds by venus resource id, so the slot must have the same
        // importable dedicated backing a direct primary has: a whole venus
        // memory (offset 0) with a live resid. Same gates as
        // finish_wddm_tex2d's backing classification.
        let (memory, memory_size, memory_offset, resid) = dxvk_resource_memory_info(h, &res);
        // Exact vkAllocateMemory identity where recorded (the C1 source);
        // the memory-info blob size is the fallback, mirroring the
        // open-path's preference order in resource.rs.
        let (mut alloc_size, mut memory_type_index) = (0u64, 0u32);
        // SAFETY: `res` is the live resource created above.
        let alloc_identity_known = unsafe {
            dev.dxvk.get_resource_alloc_identity(
                res.as_raw() as usize,
                &mut alloc_size,
                &mut memory_type_index,
                core::ptr::null_mut(),
            )
        };
        if !alloc_identity_known || alloc_size == 0 {
            alloc_size = memory_size;
        }
        let pitch = row_pitch as u32;
        if memory == 0 || memory_offset != 0 || resid == 0 || pitch == 0 || alloc_size == 0 {
            let n = SNAP_RING_CREATE_FAILS.fetch_add(1, Ordering::Relaxed);
            if n < 16 || n % 512 == 0 {
                log_error!(
                    "scanout-snapshot: ring slot {i} has no bindable identity \
                     memory=0x{:x} offset={} resid={} pitch={} alloc={} ({})",
                    memory,
                    memory_offset,
                    resid,
                    pitch,
                    alloc_size,
                    counter_summary()
                );
            }
            // `res` drops here, releasing the slot; `slots` drops the rest.
            return None;
        }
        // The ring owns the reference from here; into_raw hands it over so
        // the adopted wrapper does not release it (the PresentSrcEntry
        // precedent — SnapshotSlot::drop is the single release site).
        slots.push(SnapshotSlot {
            resource_raw: res.into_raw() as usize,
            resid,
            pitch,
            plane_offset,
            alloc_size,
            memory_type_index,
            alloc_identity_known,
        });
    }
    log_error!(
        "scanout-snapshot: ring built purpose={:?} {}x{} src_fmt={} scanout_fmt={} resids=[{}, {}, {}, {}] pitch={}",
        purpose,
        width,
        height,
        source_dxgi_format,
        scanout_dxgi_format,
        slots[0].resid,
        slots[1].resid,
        slots[2].resid,
        slots[3].resid,
        slots[0].pitch,
    );
    Some(SnapshotRing {
        width,
        height,
        source_dxgi_format,
        scanout_dxgi_format,
        purpose,
        slots,
        next: 0,
    })
}

fn make_cache_room(dev: &HeliosDevice, cache: &mut SnapshotRingCache, additional_bytes: u64) -> bool {
    while cache.rings.len() >= SNAPSHOT_CACHE_MAX_RINGS
        || cache.bytes.saturating_add(additional_bytes) > SNAPSHOT_CACHE_MAX_BYTES
    {
        let Some(index) = cache.rings.iter().position(|ring| {
            ring.purpose == SnapshotPurpose::WindowedBlt
                && ring.slots.iter().all(|slot| {
                    crate::scanout_acquire::windowed_snapshot_idle(dev, slot.resid)
                })
        }) else {
            return false;
        };
        // No future Present can select the removed ring. The KMD query covers
        // prior consumers; any queued DXVK copy retains its image references.
        let ring = cache.rings.swap_remove(index);
        cache.bytes = cache.bytes.saturating_sub(ring.byte_size());
        drop(ring);
        let n = SNAP_RING_RECLAIMS.fetch_add(1, Ordering::Relaxed) + 1;
        if n <= 16 || n % 512 == 0 {
            log_error!("scanout-snapshot: reclaimed idle ring ({})", counter_summary());
        }
    }
    true
}

/// Record the snapshot before the frame submission gate. A missing plan is
/// fatal when the source requires sample/color normalization.
///
/// # Safety
/// `h`/`src_h` are the live device/source handles of the present in progress.
pub(crate) unsafe fn snapshot_for_present(
    h: Hdevice,
    src_h: ddi::D3D10DDI_HRESOURCE,
    width: u32,
    height: u32,
    dxgi_format: u32,
    purpose: SnapshotPurpose,
) -> Option<SnapshotPlan> {
    // The knob controls optional snapshot isolation. Sample/color normalization
    // is a correctness requirement even with that optimization disabled. The
    // caller fails Present if the required capability/copy is unavailable.
    // SAFETY: src_h is the live source of this DDI call.
    let required = unsafe { requires_present_snapshot(src_h) };
    if (!crate::scanout_snapshot_knob() && !required)
        || !(match purpose {
            SnapshotPurpose::DirectFlip => crate::scanout_acquire::scanout_snapshot_capable(),
            SnapshotPurpose::WindowedBlt => crate::scanout_acquire::windowed_blt_snapshot_capable(),
        })
    {
        return None;
    }
    let dev = helios_device(h)?;
    if width == 0 || height == 0 {
        // A valid private with zero extent cannot exist (finish_wddm_tex2d
        // mints from a real mip0); refuse rather than build a 0x0 ring.
        let n = SNAP_COPY_FAILS.fetch_add(1, Ordering::Relaxed);
        if n < 16 || n % 512 == 0 {
            log_error!(
                "scanout-snapshot: refused zero-extent private {}x{} ({})",
                width,
                height,
                counter_summary()
            );
        }
        return None;
    }
    let Some(scanout_dxgi_format) = snapshot_scanout_format(dxgi_format) else {
        let n = SNAP_RING_CREATE_FAILS.fetch_add(1, Ordering::Relaxed);
        if n < 16 || n % 512 == 0 {
            log_error!(
                "scanout-snapshot: unsupported source format {} ({})",
                dxgi_format,
                counter_summary()
            );
        }
        return None;
    };

    let mut cache = dev.owned.snapshot_rings.borrow_mut();
    let mut ring_index = cache.rings.iter().position(|ring| {
        ring.width == width
            && ring.height == height
            && ring.source_dxgi_format == dxgi_format
            && ring.purpose == purpose
    });
    if ring_index.is_none() {
        let key = (width, height, dxgi_format, purpose);
        if cache.oversized_geometry == Some(key) || !make_cache_room(dev, &mut cache, 0) {
            let n = SNAP_CACHE_REFUSALS.fetch_add(1, Ordering::Relaxed);
            if n < 16 || n % 512 == 0 {
                log_error!(
                    "scanout-snapshot: cache limit refuses purpose={:?} geometry {}x{} fmt={} \
                     rings={} bytes={} limits={}/{} ({})",
                    purpose,
                    width,
                    height,
                    dxgi_format,
                    cache.rings.len(),
                    cache.bytes,
                    SNAPSHOT_CACHE_MAX_RINGS,
                    SNAPSHOT_CACHE_MAX_BYTES,
                    counter_summary()
                );
            }
            return None;
        }
        let Some(ring) = build_ring(
            dev,
            h,
            width,
            height,
            dxgi_format,
            scanout_dxgi_format,
            purpose,
        ) else {
            return None; // counted + logged in build_ring
        };
        let ring_bytes = ring.byte_size();
        if ring_bytes > SNAPSHOT_CACHE_MAX_BYTES {
            // This geometry cannot fit even in an empty cache. Avoid allocating
            // four doomed images again on each optional-snapshot Present.
            cache.oversized_geometry = Some(key);
        }
        if ring_bytes > SNAPSHOT_CACHE_MAX_BYTES
            || !make_cache_room(dev, &mut cache, ring_bytes)
        {
            let n = SNAP_CACHE_REFUSALS.fetch_add(1, Ordering::Relaxed);
            if n < 16 || n % 512 == 0 {
                log_error!(
                    "scanout-snapshot: cache byte limit refuses purpose={:?} {}x{} fmt={} \
                     ring_bytes={} retained={} limit={} ({})",
                    purpose,
                    width,
                    height,
                    dxgi_format,
                    ring_bytes,
                    cache.bytes,
                    SNAPSHOT_CACHE_MAX_BYTES,
                    counter_summary()
                );
            }
            return None;
        }
        cache.bytes += ring_bytes;
        cache.rings.push(ring);
        ring_index = Some(cache.rings.len() - 1);
    }
    // Rotate to the next slot and copy its (all-Copy) identity out, so the
    // borrow arithmetic below stays trivial.
    let (slot_index, dst_raw, plan, alloc_identity_known) = {
        let ring = cache.rings.get_mut(ring_index?)?;
        let index = ring.next % SNAPSHOT_RING_SLOTS;
        ring.next = (index + 1) % SNAPSHOT_RING_SLOTS;
        let slot = &ring.slots[index];
        (
            index,
            slot.resource_raw,
            SnapshotPlan {
                resid: slot.resid,
                width: ring.width,
                height: ring.height,
                pitch: slot.pitch,
                plane_offset: slot.plane_offset,
                source_dxgi_format: ring.source_dxgi_format,
                dxgi_format: ring.scanout_dxgi_format,
                venus_alloc_size: slot.alloc_size,
                memory_type_index: slot.memory_type_index,
                purpose,
            },
            slot.alloc_identity_known,
        )
    };

    if purpose == SnapshotPurpose::WindowedBlt && !alloc_identity_known {
        let n = SNAP_COPY_FAILS.fetch_add(1, Ordering::Relaxed);
        if n < 16 || n % 512 == 0 {
            log_error!(
                "scanout-snapshot: WindowedBlt refused snapshot slot {} without exact allocation identity ({})",
                slot_index,
                counter_summary()
            );
        }
        return None;
    }

    // Copy direction: S_i <- the PRESENTED source resource — the same COM
    // identity publish_present_order names on the flip path.
    let src_raw = resource_com_raw(src_h);
    if src_raw == 0 {
        let n = SNAP_COPY_FAILS.fetch_add(1, Ordering::Relaxed);
        if n < 16 || n % 512 == 0 {
            log_error!(
                "scanout-snapshot: presented source has no COM resource ({})",
                counter_summary()
            );
        }
        return None;
    }
    // SAFETY: `dst_raw` is the ring's owned live ID3D11Resource (the ring
    // outlives this call — it is only torn down above, under the same
    // RefCell, or at device teardown); `src_raw` is the presented resource's
    // live COM pointer for the duration of this DDI call.
    match unsafe {
        dev.dxvk.present_snapshot_copy(
            DstRes(dst_raw),
            SrcRes(src_raw),
            purpose == SnapshotPurpose::WindowedBlt,
        )
    } {
        0 => {
            let n = SNAP_SUBSTITUTED.fetch_add(1, Ordering::Relaxed);
            // Steady-state cadence: first presents prove the arm is live, then
            // ~one line per 16k presents keeps the census visible without the
            // per-512 chatter of the bring-up build (T2's log-growth budget).
            if n < 2 || (n + 1) % 16384 == 0 {
                log_error!(
                    "scanout-snapshot present #{}: slot={} resid={} {}x{} pitch={} alloc={} ({})",
                    n + 1,
                    slot_index,
                    plan.resid,
                    plan.width,
                    plan.height,
                    plan.pitch,
                    plan.venus_alloc_size,
                    counter_summary()
                );
            }
            Some(plan)
        }
        1 => {
            // The ring matches the PRIVATE geometry but the resource's
            // own extent disagreed. The bridge refused the copy before
            // recording work, so the slot must not be bound. The ring is KEPT —
            // its key still matches the private, so tearing it down would
            // rebuild an identical ring every present (a create storm); if
            // this state persists the counter and line below stay loud.
            let n = SNAP_COPY_FAILS.fetch_add(1, Ordering::Relaxed);
            if n < 16 || n % 512 == 0 {
                log_error!(
                    "scanout-snapshot: blit extent mismatch vs private {}x{} — \
                     substitution skipped ({})",
                    width,
                    height,
                    counter_summary()
                );
            }
            None
        }
        rc => {
            let n = SNAP_COPY_FAILS.fetch_add(1, Ordering::Relaxed);
            if n < 16 || n % 512 == 0 {
                log_error!(
                    "scanout-snapshot: blit FAILED rc={} slot={} resid={} ({})",
                    rc,
                    slot_index,
                    plan.resid,
                    counter_summary()
                );
            }
            None
        }
    }
}

/// The descriptor override, in `finish_present`, before BOTH consumers of the
/// owned local (`pPrivateDriverData` and the `HeliosPresentRenderCmd`).
///
/// The stale-descriptor guard, second leg: the plan only exists if the blit
/// was recorded this present (first leg, by construction), and here the
/// private data — re-read inside `finish_present` — must still describe the
/// geometry the blit was recorded against, or the override is refused. It
/// mutates the LOCAL only; `ResourceState::present_private` and
/// `direct_scanout_allocations` are rotation-coupled state and are never
/// touched.
pub(crate) fn apply_snapshot_override(
    present_private: &mut Option<HeliosPresentPrivateData>,
    plan: &SnapshotPlan,
) -> bool {
    if plan.purpose == SnapshotPurpose::WindowedBlt {
        // Windowed DXGI Present has no direct primary private record. Build a
        // fresh typed *source* descriptor, which is deliberately not marked
        // DIRECT_SCANOUT and can therefore never select scanout in the KMD.
        *present_private = Some(HeliosPresentPrivateData {
            plane_offset: plan.plane_offset,
            magic: HELIOS_PRESENT_PRIVATE_MAGIC,
            version: HELIOS_PRESENT_PRIVATE_VERSION,
            resource_id: plan.resid,
            width: plan.width,
            height: plan.height,
            pitch: plan.pitch,
            dxgi_format: plan.dxgi_format,
            reserved: HELIOS_PRESENT_PRIVATE_FLAG_SNAPSHOT
                | HELIOS_PRESENT_PRIVATE_FLAG_WINDOWED_BLT_SNAPSHOT,
            venus_alloc_size: plan.venus_alloc_size,
            present_ctx_id: 0,
            present_value: 0,
            present_cookie: 0,
            snapshot_memory_type_index: plan.memory_type_index,
            snapshot_purpose: HELIOS_PRESENT_SNAPSHOT_PURPOSE_WINDOWED_BLT,
        });
        return true;
    }
    let Some(private) = present_private.as_mut() else {
        let n = SNAP_PRIVATE_SKIPS.fetch_add(1, Ordering::Relaxed);
        if n < 16 || n % 512 == 0 {
            log_error!(
                "scanout-snapshot: blit recorded but the present carries no private data — \
                 override refused ({})",
                counter_summary()
            );
        }
        return false;
    };
    if private.width != plan.width
        || private.height != plan.height
        || private.dxgi_format != plan.source_dxgi_format
    {
        let n = SNAP_PRIVATE_SKIPS.fetch_add(1, Ordering::Relaxed);
        if n < 16 || n % 512 == 0 {
            log_error!(
                "scanout-snapshot: private geometry moved under the present \
                 ({}x{} fmt={} vs plan {}x{} fmt={}) — override refused ({})",
                private.width,
                private.height,
                private.dxgi_format,
                plan.width,
                plan.height,
                plan.source_dxgi_format,
                counter_summary()
            );
        }
        return false;
    }
    private.resource_id = plan.resid;
    private.width = plan.width;
    private.height = plan.height;
    private.pitch = plan.pitch;
    private.plane_offset = plan.plane_offset;
    private.dxgi_format = plan.dxgi_format;
    private.venus_alloc_size = plan.venus_alloc_size;
    private.reserved |= HELIOS_PRESENT_PRIVATE_FLAG_SNAPSHOT;
    private.snapshot_memory_type_index = plan.memory_type_index;
    private.snapshot_purpose = HELIOS_PRESENT_SNAPSHOT_PURPOSE_NONE;
    true
}

#[cfg(test)]
mod tests {
    use super::snapshot_scanout_format;

    #[test]
    fn packed_ten_bit_snapshot_converts_to_rgba8() {
        assert_eq!(snapshot_scanout_format(24), Some(28));
    }

    #[test]
    fn native_scanout_formats_are_preserved() {
        assert_eq!(snapshot_scanout_format(28), Some(28));
        assert_eq!(snapshot_scanout_format(87), Some(87));
        assert_eq!(snapshot_scanout_format(88), Some(88));
    }

    #[test]
    fn srgb_snapshots_use_matching_encoded_unorm_bytes() {
        assert_eq!(snapshot_scanout_format(29), Some(28));
        assert_eq!(snapshot_scanout_format(91), Some(87));
        assert_eq!(snapshot_scanout_format(93), Some(88));
    }

    #[test]
    fn unsupported_snapshot_formats_fail_closed() {
        assert_eq!(snapshot_scanout_format(10), None);
    }
}
