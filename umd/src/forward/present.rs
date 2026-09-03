//! The DXGI present path, and the DXGI DDIs that ride with it.
//!
//! `finish_present` and `dxgi_present`, the ordinary runtime callback, the
//! readback and force-opaque debug instruments, resource-identity rotation,
//! Blt/Blt1, offer/reclaim, the multiplane-overlay surface and Present1.
//!
//! Moved verbatim out of `forward.rs` by T8/R1107.

use super::*;

// --- DXGI present -----------------------------------------------------------

pub(crate) unsafe fn dxgi_device_handle(h: ddi::DXGI_DDI_HDEVICE) -> Hdevice {
    Hdevice {
        pDrvPrivate: h as *mut c_void,
    }
}

pub(crate) unsafe fn dxgi_resource_handle(h: ddi::DXGI_DDI_HRESOURCE) -> ddi::D3D10DDI_HRESOURCE {
    ddi::D3D10DDI_HRESOURCE {
        pDrvPrivate: h as *mut c_void,
    }
}

pub(crate) unsafe fn maybe_log_present_readback(h: Hdevice, src_h: ddi::D3D10DDI_HRESOURCE) {
    if !present_readback_enabled() {
        return;
    }
    let n = PRESENT_READBACK_LOG_COUNT.next();
    if n >= 8 {
        return;
    }
    let Some(device) = d3d11_device(h) else {
        log_error!("DXGI Present readback: no D3D11 device");
        return;
    };
    let Some(context) = d3d11_context(h) else {
        log_error!("DXGI Present readback: no D3D11 context");
        return;
    };
    let Some(res) = load_resource(src_h) else {
        log_error!("DXGI Present readback: source resource missing");
        return;
    };
    let Ok(tex) = (*res).cast::<ID3D11Texture2D>() else {
        log_error!("DXGI Present readback: source is not Texture2D");
        return;
    };
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    tex.GetDesc(&mut desc);
    if desc.Width == 0 || desc.Height == 0 || desc.SampleDesc.Count != 1 {
        log_error!(
            "DXGI Present readback: unsupported {}x{} fmt={} sample={}x{}",
            desc.Width,
            desc.Height,
            desc.Format.0,
            desc.SampleDesc.Count,
            desc.SampleDesc.Quality
        );
        return;
    }

    let mut staging_desc = desc;
    staging_desc.MipLevels = 1;
    staging_desc.ArraySize = 1;
    staging_desc.Usage = D3D11_USAGE_STAGING;
    staging_desc.BindFlags = 0;
    staging_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
    staging_desc.MiscFlags = 0;
    let mut staging: Option<ID3D11Texture2D> = None;
    if let Err(e) = device.CreateTexture2D(&staging_desc, None, Some(&mut staging)) {
        log_error!("DXGI Present readback: staging create failed {e:?}");
        return;
    }
    let Some(staging) = staging else {
        log_error!("DXGI Present readback: staging create returned None");
        return;
    };
    let Ok(staging_res) = staging.cast::<ID3D11Resource>() else {
        log_error!("DXGI Present readback: staging cast failed");
        return;
    };
    context.CopyResource(&staging_res, &*res);

    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    if let Err(e) = context.Map(&staging_res, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) {
        log_error!("DXGI Present readback: map failed {e:?}");
        return;
    }
    let bpp = dxgi_bytes_per_pixel(desc.Format.0 as u32).max(1) as usize;
    let row_pitch = mapped.RowPitch as usize;
    // `dxgi_bytes_per_pixel` is a PITCH-PADDING estimate, not a true bpp: its
    // 4-byte default covers the genuinely 16-bpp B5G6R5 / B5G5R5A1 / B4G4R4A4
    // formats and every block-compressed format. Over-reporting is harmless
    // where it is used to pad `linear_size`, but here it is a byte-addressing
    // stride, and `(Width - 1) * bpp` then runs past the row -- on the LAST
    // row, past the end of the mapping. `maybe_force_present_alpha_opaque`
    // already guards its own indexing with a hard `bpp != 4`; this is the
    // same refusal, expressed against the pitch the runtime actually mapped
    // so every currently-correct width/format still reads.
    let last_sample_end = (desc.Width.saturating_sub(1) as usize)
        .saturating_mul(bpp)
        .saturating_add(bpp.min(4));
    if row_pitch == 0 || last_sample_end > row_pitch {
        note_ddi_refusal(&DDI_REFUSALS.readback_stride_unsafe);
        log_error!(
            "DXGI Present readback: stride would leave the mapping, refusing \
             {}x{} fmt={} bpp={} row_pitch={} last_sample_end={}",
            desc.Width,
            desc.Height,
            desc.Format.0,
            bpp,
            row_pitch,
            last_sample_end
        );
        context.Unmap(&staging_res, 0);
        return;
    }
    let data = mapped.pData as *const u8;
    let mut sum: u64 = 0;
    let mut nonzero = 0u32;
    for y in 0..4u32 {
        for x in 0..4u32 {
            let sx = ((desc.Width - 1) as u64 * x as u64 / 3) as usize;
            let sy = ((desc.Height - 1) as u64 * y as u64 / 3) as usize;
            let p = data.add(sy * row_pitch + sx * bpp);
            let mut px = 0u32;
            for c in 0..bpp.min(4) {
                let v = *p.add(c) as u32;
                px |= v << (c * 8);
                sum += v as u64;
            }
            if px != 0 {
                nonzero += 1;
            }
        }
    }
    let cx = (desc.Width / 2) as usize;
    let cy = (desc.Height / 2) as usize;
    let cp = data.add(cy * row_pitch + cx * bpp);
    let mut center = 0u32;
    for c in 0..bpp.min(4) {
        center |= (*cp.add(c) as u32) << (c * 8);
    }
    let mut frame_sum: u64 = 0;
    let mut frame_nonzero = 0u64;
    if std::env::var_os("HELIOS_PRESENT_DUMP_DIR").is_some() {
        for y in 0..desc.Height as usize {
            for x in 0..desc.Width as usize {
                let p = data.add(y * row_pitch + x * bpp);
                let mut px = 0u32;
                for c in 0..bpp.min(4) {
                    let v = *p.add(c) as u32;
                    px |= v << (c * 8);
                    frame_sum = frame_sum.wrapping_add(v as u64);
                }
                if px != 0 {
                    frame_nonzero += 1;
                }
            }
        }
        if bpp >= 4 {
            if let Some(dir) = std::env::var_os("HELIOS_PRESENT_DUMP_DIR") {
                let _ = std::fs::create_dir_all(&dir);
                let pid = std::process::id();
                let path = std::path::PathBuf::from(dir).join(format!(
                    "present-{pid}-{:03}-{}x{}-fmt{}.bmp",
                    n + 1,
                    desc.Width,
                    desc.Height,
                    desc.Format.0
                ));
                if let Err(e) = write_bgra32_bmp(&path, data, row_pitch, desc.Width, desc.Height) {
                    log_error!("DXGI Present readback dump failed: {e}");
                } else {
                    log_error!("DXGI Present readback dump: {}", path.display());
                }
            }
        } else {
            log_error!(
                "DXGI Present readback dump skipped: bpp={} unsupported",
                bpp
            );
        }
    }
    context.Unmap(&staging_res, 0);
    log_error!(
        "DXGI Present readback #{}: {}x{} fmt={} bpp={} grid_sum={} nonzero={} center=0x{:08x} frame_sum={} frame_nonzero={}",
        n + 1,
        desc.Width,
        desc.Height,
        desc.Format.0,
        bpp,
        sum,
        nonzero,
        center,
        frame_sum,
        frame_nonzero
    );
}

pub(crate) unsafe fn write_bgra32_bmp(
    path: &std::path::Path,
    data: *const u8,
    row_pitch: usize,
    width: u32,
    height: u32,
) -> std::io::Result<()> {
    use std::io::Write;

    let row_bytes = width as usize * 4;
    let image_size = row_bytes * height as usize;
    let file_size = 14usize + 40usize + image_size;

    let mut file = std::fs::File::create(path)?;
    file.write_all(b"BM")?;
    file.write_all(&(file_size as u32).to_le_bytes())?;
    file.write_all(&0u16.to_le_bytes())?;
    file.write_all(&0u16.to_le_bytes())?;
    file.write_all(&54u32.to_le_bytes())?;

    file.write_all(&40u32.to_le_bytes())?;
    file.write_all(&(width as i32).to_le_bytes())?;
    // Negative height stores top-down rows, matching D3D's mapped row order.
    file.write_all(&(-(height as i32)).to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?;
    file.write_all(&32u16.to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?;
    file.write_all(&(image_size as u32).to_le_bytes())?;
    file.write_all(&2835i32.to_le_bytes())?;
    file.write_all(&2835i32.to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?;

    for y in 0..height as usize {
        let row = std::slice::from_raw_parts(data.add(y * row_pitch), row_bytes);
        file.write_all(row)?;
    }

    Ok(())
}

pub(crate) unsafe fn maybe_force_present_alpha_opaque(h: Hdevice, src_h: ddi::D3D10DDI_HRESOURCE) {
    if !present_force_opaque_enabled() {
        return;
    }

    let n = PRESENT_FORCE_OPAQUE_LOG_COUNT.next();
    let Some(device) = d3d11_device(h) else {
        if n < 8 {
            log_error!("DXGI Present force-opaque: no D3D11 device");
        }
        return;
    };
    let Some(context) = d3d11_context(h) else {
        if n < 8 {
            log_error!("DXGI Present force-opaque: no D3D11 context");
        }
        return;
    };
    let Some(res) = load_resource(src_h) else {
        if n < 8 {
            log_error!("DXGI Present force-opaque: source resource missing");
        }
        return;
    };
    let Ok(tex) = (*res).cast::<ID3D11Texture2D>() else {
        if n < 8 {
            log_error!("DXGI Present force-opaque: source is not Texture2D");
        }
        return;
    };

    let mut desc = D3D11_TEXTURE2D_DESC::default();
    tex.GetDesc(&mut desc);
    let bpp = dxgi_bytes_per_pixel(desc.Format.0 as u32);
    if desc.Width == 0 || desc.Height == 0 || desc.SampleDesc.Count != 1 || bpp != 4 {
        if n < 8 {
            log_error!(
                "DXGI Present force-opaque: unsupported {}x{} fmt={} bpp={} sample={}x{}",
                desc.Width,
                desc.Height,
                desc.Format.0,
                bpp,
                desc.SampleDesc.Count,
                desc.SampleDesc.Quality
            );
        }
        return;
    }

    let mut staging_desc = desc;
    staging_desc.MipLevels = 1;
    staging_desc.ArraySize = 1;
    staging_desc.Usage = D3D11_USAGE_STAGING;
    staging_desc.BindFlags = 0;
    staging_desc.CPUAccessFlags = (D3D11_CPU_ACCESS_READ.0 | D3D11_CPU_ACCESS_WRITE.0) as u32;
    staging_desc.MiscFlags = 0;

    let mut staging: Option<ID3D11Texture2D> = None;
    if let Err(e) = device.CreateTexture2D(&staging_desc, None, Some(&mut staging)) {
        if n < 8 {
            log_error!("DXGI Present force-opaque: staging create failed {e:?}");
        }
        return;
    }
    let Some(staging) = staging else {
        if n < 8 {
            log_error!("DXGI Present force-opaque: staging create returned None");
        }
        return;
    };
    let Ok(staging_res) = staging.cast::<ID3D11Resource>() else {
        if n < 8 {
            log_error!("DXGI Present force-opaque: staging cast failed");
        }
        return;
    };

    context.CopyResource(&staging_res, &*res);

    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    if let Err(e) = context.Map(&staging_res, 0, D3D11_MAP_READ_WRITE, 0, Some(&mut mapped)) {
        if n < 8 {
            log_error!("DXGI Present force-opaque: map failed {e:?}");
        }
        return;
    }

    let row_pitch = mapped.RowPitch as usize;
    let data = mapped.pData as *mut u8;
    let mut alpha_zero = 0u64;
    let mut alpha_non_opaque = 0u64;
    for y in 0..desc.Height as usize {
        for x in 0..desc.Width as usize {
            let alpha = data.add(y * row_pitch + x * 4 + 3);
            let old = *alpha;
            if old == 0 {
                alpha_zero += 1;
            }
            if old != 0xff {
                alpha_non_opaque += 1;
                *alpha = 0xff;
            }
        }
    }
    context.Unmap(&staging_res, 0);
    context.CopyResource(&*res, &staging_res);
    context.Flush();

    if n < 8 || (n + 1) % 512 == 0 {
        log_error!(
            "DXGI Present force-opaque #{}: {}x{} fmt={} alpha_zero={} alpha_non_opaque={}",
            n + 1,
            desc.Width,
            desc.Height,
            desc.Format.0,
            alpha_zero,
            alpha_non_opaque
        );
    }
}

/// The DXGI entry which led to a runtime present callback.
///
/// This is deliberately observation-only.  In particular, `Present1Single`
/// still takes the ordinary Present implementation after the DDI argument is
/// translated; retaining the tag is what makes that delegation visible in the
/// per-process probe summary.
#[derive(Clone, Copy)]
pub(crate) enum PresentBoundaryEntry {
    Present,
    Present1Single,
    Present1Multi,
    Mpo,
}

impl PresentBoundaryEntry {
    const fn name(self) -> &'static str {
        match self {
            Self::Present => "Present",
            Self::Present1Single => "Present1-single",
            Self::Present1Multi => "Present1-multi",
            Self::Mpo => "MPO",
        }
    }

    fn entry_counter(self) -> &'static AtomicUsize {
        match self {
            Self::Present => &PRESENT_BOUNDARY_PRESENT,
            Self::Present1Single => &PRESENT_BOUNDARY_PRESENT1_SINGLE,
            Self::Present1Multi => &PRESENT_BOUNDARY_PRESENT1_MULTI,
            Self::Mpo => &PRESENT_BOUNDARY_MPO,
        }
    }
}

// These are deliberately process-global: DXGI owns the DDI entry lifetime,
// while a process may create and destroy more than one UMD device. The probe
// uses relaxed counters only; allocation/string formatting happens in the
// first-N diagnostic lines or the PASSIVE DestroyDevice summary, never in the
// steady-state callback path.
static PRESENT_BOUNDARY_PRESENT: AtomicUsize = AtomicUsize::new(0);
static PRESENT_BOUNDARY_PRESENT1_SINGLE: AtomicUsize = AtomicUsize::new(0);
static PRESENT_BOUNDARY_PRESENT1_MULTI: AtomicUsize = AtomicUsize::new(0);
static PRESENT_BOUNDARY_MPO: AtomicUsize = AtomicUsize::new(0);
static PRESENT_BOUNDARY_PRESENT_ATTEMPTED: AtomicUsize = AtomicUsize::new(0);
static PRESENT_BOUNDARY_PRESENT_SUCCESS: AtomicUsize = AtomicUsize::new(0);
static PRESENT_BOUNDARY_PRESENT_FAILURE: AtomicUsize = AtomicUsize::new(0);
static PRESENT_BOUNDARY_PRESENT_MISSING: AtomicUsize = AtomicUsize::new(0);
static PRESENT_BOUNDARY_MPO_ATTEMPTED: AtomicUsize = AtomicUsize::new(0);
static PRESENT_BOUNDARY_MPO_SUCCESS: AtomicUsize = AtomicUsize::new(0);
static PRESENT_BOUNDARY_MPO_FAILURE: AtomicUsize = AtomicUsize::new(0);
static PRESENT_BOUNDARY_MPO_MISSING: AtomicUsize = AtomicUsize::new(0);
static PRESENT_BOUNDARY_EARLY_REFUSAL: AtomicUsize = AtomicUsize::new(0);
static PRESENT_BOUNDARY_ENTRY_LOG_COUNT: LogThrottle = LogThrottle::new();
static PRESENT_BOUNDARY_CALLBACK_LOG_COUNT: LogThrottle = LogThrottle::new();
static PRESENT_BOUNDARY_REFUSAL_LOG_COUNT: LogThrottle = LogThrottle::new();

const PRESENT_BOUNDARY_LOG_FIRST_N: usize = 16;

fn probe_entry_attempt(entry: PresentBoundaryEntry) {
    entry.entry_counter().fetch_add(1, Ordering::Relaxed);
}

unsafe fn probe_present_entry(
    entry: PresentBoundaryEntry,
    a: &ddi::DXGI_DDI_ARG_PRESENT,
    src_alloc: ddi::D3DKMT_HANDLE,
    dst_alloc: ddi::D3DKMT_HANDLE,
) {
    let Some(n) = PRESENT_BOUNDARY_ENTRY_LOG_COUNT.first_n(PRESENT_BOUNDARY_LOG_FIRST_N) else {
        return;
    };
    let flags = *(&a.Flags as *const ddi::DXGI_DDI_PRESENT_FLAGS as *const u32);
    log_error!(
        "present-boundary entry #{} {}: callback_src=0 hSrc={:p}/{} srcAlloc=0x{:x} \
         hDst={:p}/{} dstAlloc=0x{:x} flags=0x{:x} dirty=0 multiplicity=1 dxgiCtx={:p}",
        n + 1,
        entry.name(),
        a.hSurfaceToPresent as *mut c_void,
        a.SrcSubResourceIndex,
        src_alloc,
        a.hDstResource as *mut c_void,
        a.DstSubResourceIndex,
        dst_alloc,
        flags,
        a.pDXGIContext,
    );
}

unsafe fn probe_present1_multi_entry(
    a: &ddi::DXGI_DDI_ARG_PRESENT1,
    callback_source_index: usize,
    source_handle: usize,
    source_subresource: u32,
    src_alloc: ddi::D3DKMT_HANDLE,
    dst_alloc: ddi::D3DKMT_HANDLE,
) {
    let Some(n) = PRESENT_BOUNDARY_ENTRY_LOG_COUNT.first_n(PRESENT_BOUNDARY_LOG_FIRST_N) else {
        return;
    };
    let flags = *(&a.Flags as *const ddi::DXGI_DDI_PRESENT_FLAGS as *const u32);
    log_error!(
        "present-boundary entry #{} Present1-multi: surfaces={} callback_src={} hSrc={:p}/{} \
         srcAlloc=0x{:x} hDst={:p}/{} dstAlloc=0x{:x} flags=0x{:x} dirty={} multiplicity={} dxgiCtx={:p}",
        n + 1,
        a.SurfacesToPresent,
        callback_source_index,
        source_handle as *mut c_void,
        source_subresource,
        src_alloc,
        a.hDstResource as *mut c_void,
        a.DstSubResourceIndex,
        dst_alloc,
        flags,
        a.DirtyRects,
        a.BackBufferMultiplicity,
        a.pDXGIContext,
    );
}

fn probe_early_refusal(entry: PresentBoundaryEntry, reason: &'static str) {
    PRESENT_BOUNDARY_EARLY_REFUSAL.fetch_add(1, Ordering::Relaxed);
    if let Some(n) = PRESENT_BOUNDARY_REFUSAL_LOG_COUNT.first_n(PRESENT_BOUNDARY_LOG_FIRST_N) {
        log_error!(
            "present-boundary refusal #{} {}: {}",
            n + 1,
            entry.name(),
            reason
        );
    }
}

unsafe fn probe_present_result(
    entry: PresentBoundaryEntry,
    hr: i32,
    callback_args: &ddi::DXGIDDICB_PRESENT,
    flags: u32,
) {
    PRESENT_BOUNDARY_PRESENT_ATTEMPTED.fetch_add(1, Ordering::Relaxed);
    if hr >= 0 {
        PRESENT_BOUNDARY_PRESENT_SUCCESS.fetch_add(1, Ordering::Relaxed);
    } else {
        PRESENT_BOUNDARY_PRESENT_FAILURE.fetch_add(1, Ordering::Relaxed);
    }
    if let Some(n) = PRESENT_BOUNDARY_CALLBACK_LOG_COUNT.first_n(PRESENT_BOUNDARY_LOG_FIRST_N) {
        log_error!(
            "present-boundary callback #{} {} pfnPresentCb: hr=0x{:08x} srcAlloc=0x{:x} dstAlloc=0x{:x} \
             hContext={:p} dxgiCtx={:p} flags=0x{:x} optimize={}",
            n + 1,
            entry.name(),
            hr as u32,
            callback_args.hSrcAllocation,
            callback_args.hDstAllocation,
            callback_args.hContext,
            callback_args.pDXGIContext,
            flags,
            callback_args.bOptimizeForComposition,
        );
    }
}

unsafe fn probe_mpo_result(hr: i32, callback_args: &ddi::DXGIDDICB_PRESENT_MULTIPLANE_OVERLAY) {
    PRESENT_BOUNDARY_MPO_ATTEMPTED.fetch_add(1, Ordering::Relaxed);
    if hr >= 0 {
        PRESENT_BOUNDARY_MPO_SUCCESS.fetch_add(1, Ordering::Relaxed);
    } else {
        PRESENT_BOUNDARY_MPO_FAILURE.fetch_add(1, Ordering::Relaxed);
    }
    if let Some(n) = PRESENT_BOUNDARY_CALLBACK_LOG_COUNT.first_n(PRESENT_BOUNDARY_LOG_FIRST_N) {
        log_error!(
            "present-boundary callback #{} MPO pfnPresentMultiplaneOverlayCb: hr=0x{:08x} allocations={} \
             hContext={:p} dxgiCtx={:p}",
            n + 1,
            hr as u32,
            callback_args.AllocationInfoCount,
            callback_args.hContext,
            callback_args.pDXGIContext,
        );
    }
}

unsafe fn probe_mpo_entry(
    a: &ddi::DXGI_DDI_ARG_PRESENTMULTIPLANEOVERLAY,
    callback_args: &ddi::DXGIDDICB_PRESENT_MULTIPLANE_OVERLAY,
) {
    let Some(n) = PRESENT_BOUNDARY_ENTRY_LOG_COUNT.first_n(PRESENT_BOUNDARY_LOG_FIRST_N) else {
        return;
    };
    log_error!(
        "present-boundary entry #{} MPO: planes={} enabled={} dxgiCtx={:p} hContext={:p}",
        n + 1,
        a.PresentPlaneCount,
        callback_args.AllocationInfoCount,
        a.pDXGIContext,
        callback_args.hContext,
    );
    let mut allocation_slot = 0usize;
    for i in 0..a.PresentPlaneCount as usize {
        let plane = &*a.pPresentPlanes.add(i);
        let allocation = if plane.Enabled == 0 {
            0
        } else {
            let allocation = callback_args.AllocationInfo[allocation_slot].PresentAllocation;
            allocation_slot += 1;
            allocation
        };
        log_error!(
            "present-boundary entry #{} MPO plane {}: enabled={} hSrc={:p}/{} srcAlloc=0x{:x} flags=0x{:x} dirty={}",
            n + 1,
            i,
            plane.Enabled,
            plane.hResource as *mut c_void,
            plane.SubResourceIndex,
            allocation,
            plane.PlaneAttributes.Flags,
            plane.PlaneAttributes.DirtyRectCount,
        );
    }
}

/// Process-global present-entry and callback totals, read at `DestroyDevice`.
pub(crate) fn present_boundary_summary() -> String {
    format!(
        "DDI PresentBoundary: entry present={} present1_single={} present1_multi={} mpo={} \
         present attempt/success/failure/missing={}/{}/{}/{} \
         mpo attempt/success/failure/missing={}/{}/{}/{} early_refusal={}",
        PRESENT_BOUNDARY_PRESENT.load(Ordering::Relaxed),
        PRESENT_BOUNDARY_PRESENT1_SINGLE.load(Ordering::Relaxed),
        PRESENT_BOUNDARY_PRESENT1_MULTI.load(Ordering::Relaxed),
        PRESENT_BOUNDARY_MPO.load(Ordering::Relaxed),
        PRESENT_BOUNDARY_PRESENT_ATTEMPTED.load(Ordering::Relaxed),
        PRESENT_BOUNDARY_PRESENT_SUCCESS.load(Ordering::Relaxed),
        PRESENT_BOUNDARY_PRESENT_FAILURE.load(Ordering::Relaxed),
        PRESENT_BOUNDARY_PRESENT_MISSING.load(Ordering::Relaxed),
        PRESENT_BOUNDARY_MPO_ATTEMPTED.load(Ordering::Relaxed),
        PRESENT_BOUNDARY_MPO_SUCCESS.load(Ordering::Relaxed),
        PRESENT_BOUNDARY_MPO_FAILURE.load(Ordering::Relaxed),
        PRESENT_BOUNDARY_MPO_MISSING.load(Ordering::Relaxed),
        PRESENT_BOUNDARY_EARLY_REFUSAL.load(Ordering::Relaxed),
    )
}

/// Which entry point a [`finish_present`] call is serving.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PresentKind {
    Present,
    Present1Multi,
}

impl PresentKind {
    /// Prefix on the PresentCb identity trace. The two spellings are load
    /// bearing: they are how a log reader tells which entry point ran.
    pub(crate) fn identity_prefix(self) -> &'static str {
        match self {
            PresentKind::Present => "DXGI ",
            PresentKind::Present1Multi => "DXGI Present1 ",
        }
    }

    /// Prefix on this tail's error lines.
    pub(crate) fn error_tag(self) -> &'static str {
        match self {
            PresentKind::Present => "DXGI Present",
            PresentKind::Present1Multi => "DXGI Present1 multi",
        }
    }

    /// What the tail returns when it never reaches the present callback --
    /// no device, or a refused prerequisite on the fall-through path.
    ///
    /// DIVERGENT AND PRESERVED: Present1-multi initialises to `E_INVALIDARG`,
    /// so a device-less path there returns a FAILURE where Present returns
    /// success.
    pub(crate) fn initial_hr(self) -> i32 {
        match self {
            PresentKind::Present => 0,
            PresentKind::Present1Multi => E_INVALIDARG,
        }
    }

    /// What a failed `present_prerequisites` check does.
    ///
    /// DIVERGENT AND PRESERVED: Present logs (rate-capped) and FALLS THROUGH
    /// to the rest of its body with `initial_hr`; Present1-multi RETURNS
    /// `DXGI_ERROR_UNSUPPORTED` immediately, skipping its trailing log. The
    /// two also log different field sets, which is why the message is emitted
    /// per kind rather than unified.
    pub(crate) fn missing_prereq_hr(self) -> Option<i32> {
        match self {
            PresentKind::Present => None,
            PresentKind::Present1Multi => Some(DXGI_ERROR_UNSUPPORTED),
        }
    }
}

/// The per-call values the shared present tail needs that are not derivable
/// from [`PresentKind`]. Both entry points read them from their own (different)
/// DDI argument struct.
pub(crate) struct PresentRequest {
    pub(crate) kind: PresentKind,
    pub(crate) boundary: PresentBoundaryEntry,
    /// `DXGI_DDI_ARG_PRESENT{,1}::pDXGIContext`, passed straight through.
    pub(crate) dxgi_context: *mut c_void,
    /// The raw `Flags` word. TRACE ONLY -- nothing branches on it here.
    pub(crate) flags: u32,
}

/// The shared tail synchronizes the normal DXVK submission path, builds the
/// ordinary runtime callback, and calls `pfnPresentCb` with no driver-private
/// payload.
pub(crate) unsafe fn finish_present(
    h: Hdevice,
    src_h: ddi::D3D10DDI_HRESOURCE,
    dst_h: ddi::D3D10DDI_HRESOURCE,
    src_alloc: u32,
    dst_alloc: u32,
    req: PresentRequest,
) -> Result<i32, i32> {
    let no_callback_hr = req.kind.initial_hr();
    let Some(dev) = helios_device(h) else {
        probe_early_refusal(req.boundary, "device handle not live");
        return Ok(no_callback_hr);
    };

    let ready = match present_prerequisites(dev, src_alloc) {
        Ok(ready) => ready,
        Err(_skip) => {
            if dev.dxgi_callbacks.is_null() {
                PRESENT_BOUNDARY_PRESENT_MISSING.fetch_add(1, Ordering::Relaxed);
            }
            probe_early_refusal(req.boundary, "present prerequisites refused");
            match req.kind {
                PresentKind::Present => {
                    if PRESENT_SKIP_LOG_COUNT
                        .first_n_then_every_from_one(64, 512)
                        .is_some()
                    {
                        log_error!(
                            "DXGI Present: skip PresentCb callbacks={} src=0x{:x} hContext={:p}",
                            dev.dxgi_callbacks.is_null(),
                            src_alloc,
                            dev.outer
                                .context
                                .as_ref()
                                .map_or(core::ptr::null_mut(), |c| c.handle.as_ptr())
                        );
                    }
                }
                PresentKind::Present1Multi => {
                    log_error!(
                        "DXGI Present1 multi: missing callback table/context callbacks={} hContext={:p}",
                        dev.dxgi_callbacks.is_null(),
                        dev.outer.context
                            .as_ref()
                            .map_or(core::ptr::null_mut(), |c| c.handle.as_ptr())
                    );
                }
            }
            return match req.kind.missing_prereq_hr() {
                Some(hr) => Err(hr),
                None => Ok(no_callback_hr),
            };
        }
    };

    if !dev.dxvk.flush_submitted() {
        probe_early_refusal(req.boundary, "DXVK submission synchronization failed");
        log_error!(
            "{}: DXVK submission synchronization failed",
            req.kind.error_tag()
        );
        return Err(E_FAIL);
    }

    let Some(present_cb) = (*dev.dxgi_callbacks).pfnPresentCb else {
        PRESENT_BOUNDARY_PRESENT_MISSING.fetch_add(1, Ordering::Relaxed);
        probe_early_refusal(req.boundary, "pfnPresentCb missing");
        log_error!("{}: pfnPresentCb missing", req.kind.error_tag());
        return Err(E_FAIL);
    };

    let mut cb = ddi::DXGIDDICB_PRESENT::default();
    cb.hSrcAllocation = ready.src_alloc.get();
    cb.hDstAllocation = dst_alloc;
    cb.pDXGIContext = req.dxgi_context;
    cb.hContext = ready.h_context.as_ptr();
    cb.BroadcastContextCount = 0;
    cb.PrivateDriverDataSize = 0;
    cb.pPrivateDriverData = core::ptr::null_mut();
    cb.bOptimizeForComposition = if present_optimize_composition_enabled() {
        1
    } else {
        0
    };

    if let Some(cb_n) = PRESENT_CB_LOG_COUNT.first_n_then_every_from_one(128, 512) {
        let (src_rt, src_km) = resource_parent_handles(src_h);
        let (dst_rt, dst_km) = resource_parent_handles(dst_h);
        trace_line!(
            "{}PresentCb identity: #{} src_alloc=0x{:x} dst_alloc=0x{:x} \
             src_hDrv={:p} src_hRT={:p} src_hKM=0x{:x} dst_hDrv={:p} \
             dst_hRT={:p} dst_hKM=0x{:x} hContext={:p} dxgi_context={:p} \
             flags=0x{:x} broadcast={} private={:p}/{} optimize={}",
            req.kind.identity_prefix(),
            cb_n,
            cb.hSrcAllocation,
            cb.hDstAllocation,
            src_h.pDrvPrivate,
            src_rt,
            src_km,
            dst_h.pDrvPrivate,
            dst_rt,
            dst_km,
            cb.hContext,
            cb.pDXGIContext,
            req.flags,
            cb.BroadcastContextCount,
            cb.pPrivateDriverData,
            cb.PrivateDriverDataSize,
            cb.bOptimizeForComposition,
        );
    }

    let trace_start_ns = dev.dxvk.feed_trace_timestamp_ns();
    let hr = present_cb(dev.h_rt_device, &mut cb);
    if trace_start_ns != 0 {
        let trace_end_ns = dev.dxvk.feed_trace_timestamp_ns();
        dev.dxvk
            .feed_trace_present_callback(trace_end_ns.saturating_sub(trace_start_ns));
    }
    probe_present_result(req.boundary, hr, &cb, req.flags);
    Ok(hr)
}
/// DXGI `pfnPresent`: copy the source resource to the destination resource when
/// DXGI provides both handles, then flush submitted GPU work.
pub(crate) unsafe extern "C" fn dxgi_present(arg: *mut ddi::DXGI_DDI_ARG_PRESENT) -> i32 {
    probe_entry_attempt(PresentBoundaryEntry::Present);
    dxgi_present_impl(arg, PresentBoundaryEntry::Present)
}

/// The common implementation for an ordinary Present and a one-surface
/// Present1 delegation. The caller chooses only the observation tag; rendering,
/// callback behavior remains the ordinary path.
unsafe fn dxgi_present_impl(
    arg: *mut ddi::DXGI_DDI_ARG_PRESENT,
    boundary: PresentBoundaryEntry,
) -> i32 {
    if arg.is_null() {
        probe_early_refusal(boundary, "null Present argument");
        return 0;
    }
    let a = &*arg;
    let flags = *(&a.Flags as *const ddi::DXGI_DDI_PRESENT_FLAGS as *const u32);
    let h = dxgi_device_handle(a.hDevice);
    let src_h = dxgi_resource_handle(a.hSurfaceToPresent);
    let dst_h = dxgi_resource_handle(a.hDstResource);
    let src_alloc = resource_allocation(src_h);
    let dst_alloc = resource_allocation(dst_h);
    probe_present_entry(boundary, a, src_alloc, dst_alloc);

    let mut copied = false;
    if let Some(context) = d3d11_context(h) {
        if let (Some(dst), Some(src)) = (load_resource(dst_h), load_resource(src_h)) {
            context.CopySubresourceRegion(&*dst, 0, 0, 0, 0, &*src, 0, None);
            copied = true;
        }
        context.Flush();
    }

    maybe_force_present_alpha_opaque(h, src_h);
    maybe_log_present_readback(h, src_h);

    let present_hr = match finish_present(
        h,
        src_h,
        dst_h,
        src_alloc,
        dst_alloc,
        PresentRequest {
            kind: PresentKind::Present,
            boundary,
            dxgi_context: a.pDXGIContext,
            flags,
        },
    ) {
        Ok(hr) => hr,
        Err(hr) => return hr,
    };

    static PRESENT_ORDINAL: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
    let ordinal = PRESENT_ORDINAL.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    if ordinal < 64 || (ordinal + 1) % 512 == 0 {
        log_error!(
            "DXGI Present: #{} src=0x{:x} dst=0x{:x} copied={} flags=0x{:x} opt_comp={} presentCb=0x{:08x} \
             hSurf={:p} srcSub={} hDstRes={:p} dstSub={} flipInterval={} dxgiCtx={:p} hContext={:p} \
             skips={}/{}/{}",
            ordinal,
            src_alloc,
            dst_alloc,
            copied,
            flags,
            present_optimize_composition_enabled() as u32,
            present_hr as u32,
            src_h.pDrvPrivate,
            a.SrcSubResourceIndex,
            dst_h.pDrvPrivate,
            a.DstSubResourceIndex,
            a.FlipInterval,
            a.pDXGIContext,
            dev_context_for_log(h),
            PRESENT_SKIP_NO_CALLBACKS.load(Ordering::Relaxed),
            PRESENT_SKIP_NO_CONTEXT.load(Ordering::Relaxed),
            PRESENT_SKIP_NO_SRC_ALLOC.load(Ordering::Relaxed),
        );
    }
    present_hr
}
// Best-effort context handle for present logging (null when unavailable).
pub(crate) fn dev_context_for_log(h: ddi::D3D10DDI_HDEVICE) -> *mut core::ffi::c_void {
    unsafe {
        helios_device(h).map_or(core::ptr::null_mut(), |d| {
            d.outer
                .context
                .as_ref()
                .map_or(core::ptr::null_mut(), |c| c.handle.as_ptr())
        })
    }
}

pub(crate) unsafe extern "C" fn dxgi_get_gamma_caps(
    arg: *mut ddi::DXGI_DDI_ARG_GET_GAMMA_CONTROL_CAPS,
) -> i32 {
    if arg.is_null() {
        return 0;
    }
    let caps = (*arg).pGammaCapabilities;
    if !caps.is_null() {
        core::ptr::write_bytes(
            caps as *mut u8,
            0,
            core::mem::size_of::<ddi::DXGI_GAMMA_CONTROL_CAPABILITIES>(),
        );
        (*caps).MaxConvertedValue = 1.0;
        (*caps).MinConvertedValue = 0.0;
    }
    0
}

pub(crate) unsafe extern "C" fn dxgi_set_display_mode(
    arg: *mut ddi::DXGI_DDI_ARG_SETDISPLAYMODE,
) -> i32 {
    if arg.is_null() {
        return E_INVALIDARG;
    }
    let a = &*arg;
    let h = dxgi_device_handle(a.hDevice);
    let Some(dev) = helios_device(h) else {
        log_error!("DXGI SetDisplayMode: missing device");
        return E_INVALIDARG;
    };
    if dev.kt_callbacks.is_null() {
        log_error!("DXGI SetDisplayMode: missing runtime callbacks");
        return E_FAIL;
    }
    let Some(set_display_mode_cb) = (*dev.kt_callbacks).pfnSetDisplayModeCb else {
        log_error!("DXGI SetDisplayMode: pfnSetDisplayModeCb missing");
        return E_FAIL;
    };

    // Windows supplies the authoritative primary resource and subresource for
    // the fullscreen transition. Translate that exact runtime resource to the
    // allocation created for it; pfnSetDisplayModeCb then asks dxgkrnl to make
    // that allocation the scan-out primary and initiates the VidPn commit.
    let resource = dxgi_resource_handle(a.hResource);
    let allocation = resource_allocation(resource);
    if allocation == 0 {
        log_error!(
            "DXGI SetDisplayMode: resource=0x{:x} sub={} has no WDDM allocation",
            a.hResource,
            a.SubResourceIndex
        );
        return E_INVALIDARG;
    }

    if let Some(context) = d3d11_context(h) {
        context.Flush();
    }
    let mut callback = ddi::D3DDDICB_SETDISPLAYMODE {
        hPrimaryAllocation: allocation,
        PrivateDriverFormatAttribute: 0,
    };
    let hr = set_display_mode_cb(dev.h_rt_device, &mut callback);
    log_error!(
        "DXGI SetDisplayMode: resource=0x{:x} sub={} allocation=0x{:x} hr=0x{:08x} private_format=0x{:x}",
        a.hResource,
        a.SubResourceIndex,
        allocation,
        hr as u32,
        callback.PrivateDriverFormatAttribute
    );
    hr
}

pub(crate) unsafe extern "C" fn dxgi_set_resource_priority(
    _arg: *mut ddi::DXGI_DDI_ARG_SETRESOURCEPRIORITY,
) -> i32 {
    0
}

pub(crate) unsafe extern "C" fn dxgi_query_resource_residency(
    arg: *mut ddi::DXGI_DDI_ARG_QUERYRESOURCERESIDENCY,
) -> i32 {
    if arg.is_null() {
        return 0;
    }
    let a = &*arg;
    if !a.pStatus.is_null() {
        for i in 0..a.Resources as usize {
            *a.pStatus.add(i) = ddi::DXGI_DDI_RESIDENCY_DXGI_DDI_RESIDENCY_FULLY_RESIDENT;
        }
    }
    0
}

/// DXGI flip-model identity rotation. The runtime calls this after each flip
/// present so the app's fixed buffer objects walk the swapchain's allocation
/// ring: resource[i] takes resource[i+1]'s identity, the last takes the
/// first's. Two coordinated moves keep the world consistent:
///   1. the DXVK storages (venus memory + VkImage + KMT handles) rotate in
///      the bridge, so draws through existing views land in the allocation
///      the runtime now associates with the buffer;
///   2. our per-resource WDDM {allocation, km} records rotate here, so the
///      next present reports the rotated hSrcAllocation to dxgkrnl.
/// The old Flush-only stub pinned dwm's composition to ONE allocation while
/// dxgkrnl/IddCx walked all three swapchain buffers — two of every three
/// acquired frames were buffers dwm never rendered (black IDD output).
/// Outcome of one swapchain identity rotation. Five exits used to `return 0`,
/// which the DXGI DDI reads as success; this names them instead.
#[derive(Copy, Clone, PartialEq, Eq)]
pub(crate) enum RotationOutcome {
    Rotated,
    /// `rotate_resource_backings` returned false — an entry with no DXVK image
    /// storage, or a `DxvkError`/unknown exception swallowed into false.
    BridgeRefused,
    /// No Helios device behind the DXGI device handle.
    NoDevice,
}

/// The DXVK backing rotation refused it.
pub(crate) static ROTATE_REFUSED: AtomicUsize = AtomicUsize::new(0);
/// A null resource handle or an untracked resource in the ring.
pub(crate) static ROTATE_UNTRACKED: AtomicUsize = AtomicUsize::new(0);
/// No Helios device behind the DXGI device handle.
pub(crate) static ROTATE_NO_DEVICE: AtomicUsize = AtomicUsize::new(0);
/// `Resources < 2` or a null array — the exit that had no log at all.
pub(crate) static ROTATE_SKIPPED: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn rotate_counter_summary() -> String {
    format!(
        "refused={} untracked={} no_device={} skipped={}",
        ROTATE_REFUSED.load(Ordering::Relaxed),
        ROTATE_UNTRACKED.load(Ordering::Relaxed),
        ROTATE_NO_DEVICE.load(Ordering::Relaxed),
        ROTATE_SKIPPED.load(Ordering::Relaxed),
    )
}

/// Both rotation phases, with NO return path between them.
///
/// The bridge rotation and the WDDM record rotation used to be two statements
/// held in the right order purely by statement order. If the bridge refuses
/// after the records moved — or vice versa — dwm composites into an allocation
/// dxgkrnl no longer scans out, which is the historical black-IDD bug this
/// DDI's own doc comment describes.
///
/// `states` is a slice of INDEPENDENT raw pointers, so a `&mut [ResourceState]`
/// cannot be formed from it and this stays `unsafe`. Panic-free: no indexing,
/// no `unwrap` — `first`/`last`/`windows` only.
pub(crate) unsafe fn rotate_ring(
    dev: &crate::device_funcs::HeliosDevice,
    states: &[*mut ResourceState],
) -> RotationOutcome {
    let (Some(&first), Some(&last)) = (states.first(), states.last()) else {
        // Unreachable: the caller validated len >= 2.
        ROTATE_SKIPPED.fetch_add(1, Ordering::Relaxed);
        return RotationOutcome::BridgeRefused;
    };

    let ptrs: Vec<usize> = states.iter().map(|s| (**s).com_raw).collect();
    if !dev.dxvk.rotate_resource_backings(ptrs.as_ptr(), ptrs.len()) {
        ROTATE_REFUSED.fetch_add(1, Ordering::Relaxed);
        return RotationOutcome::BridgeRefused;
    }

    // Rotate the WDDM identity records in lockstep with the storages.
    let first_allocation = (*first).allocation.take();
    let first_km_resource = (*first).km_resource;
    // `ownership` rotates with the allocation it describes; `rt_resource`
    // deliberately does NOT rotate and is not touched here. That asymmetry is
    // why R804 keeps the discriminant a separate field rather than bundling the
    // runtime handle into the ownership enum -- a variant carrying the handle
    // would change what RotateResourceIdentities moves.
    let first_ownership = (*first).ownership;
    // The outer token and the guest pages name THIS allocation (the token's
    // table entry is checked against the allocation handle at teardown), so
    // they travel with it. Left behind, the first post-rotation teardown
    // refused MissingToken and set device_lost (3DMark Demo, 2026-09-03).
    let first_outer_allocation = (*first).outer_allocation.take();
    let first_cpu_backing = (*first).cpu_backing.take();
    for pair in states.windows(2) {
        let (Some(&cur), Some(&next)) = (pair.first(), pair.get(1)) else {
            continue;
        };
        (*cur).allocation = (*next).allocation.take();
        (*cur).km_resource = (*next).km_resource;
        (*cur).ownership = (*next).ownership;
        (*cur).outer_allocation = (*next).outer_allocation.take();
        (*cur).cpu_backing = (*next).cpu_backing.take();
    }
    (*last).allocation = first_allocation;
    (*last).km_resource = first_km_resource;
    (*last).ownership = first_ownership;
    (*last).outer_allocation = first_outer_allocation;
    (*last).cpu_backing = first_cpu_backing;

    RotationOutcome::Rotated
}

pub(crate) unsafe extern "C" fn dxgi_rotate_resource_identities(
    arg: *mut ddi::DXGI_DDI_ARG_ROTATE_RESOURCE_IDENTITIES,
) -> i32 {
    if arg.is_null() {
        return 0;
    }
    let a = &*arg;
    let h = dxgi_device_handle(a.hDevice);
    let n = a.Resources as usize;
    if n < 2 || a.pResources.is_null() {
        let c = ROTATE_SKIPPED.fetch_add(1, Ordering::Relaxed);
        if c < 16 || c % 512 == 0 {
            log_error!(
                "DXGI RotateResourceIdentities: skipped resources={} null_array={} ({})",
                n,
                a.pResources.is_null(),
                rotate_counter_summary()
            );
        }
        return 0;
    }

    // Collect the per-resource state pointers; all entries must be tracked
    // resources or the rotation is refused whole (a partial rotation would
    // permanently corrupt the swapchain mapping).
    let mut states: Vec<*mut ResourceState> = Vec::with_capacity(n);
    for i in 0..n {
        let hr = dxgi_resource_handle(*a.pResources.add(i));
        if hr.pDrvPrivate.is_null() {
            ROTATE_UNTRACKED.fetch_add(1, Ordering::Relaxed);
            log_error!(
                "DXGI RotateResourceIdentities: null resource handle ({})",
                rotate_counter_summary()
            );
            return 0;
        }
        let state = match boxed_slot(hr) {
            Some(slot) => slot.ptr(),
            None => core::ptr::null_mut(),
        };
        if state.is_null() {
            ROTATE_UNTRACKED.fetch_add(1, Ordering::Relaxed);
            log_error!(
                "DXGI RotateResourceIdentities: untracked resource ({})",
                rotate_counter_summary()
            );
            return 0;
        }
        states.push(state);
    }

    let outcome = match helios_device(h) {
        Some(dev) => rotate_ring(dev, &states),
        None => {
            ROTATE_NO_DEVICE.fetch_add(1, Ordering::Relaxed);
            RotationOutcome::NoDevice
        }
    };
    if outcome != RotationOutcome::Rotated {
        log_error!(
            "DXGI RotateResourceIdentities: backing rotation FAILED ({})",
            rotate_counter_summary()
        );
        return 0;
    }

    if ROTATE_LOG_COUNT.first_n(64).is_some() {
        let first_handle = states.first().map_or(0, |&first| {
            (*first)
                .allocation
                .as_ref()
                .map(ResidentAllocation::handle)
                .unwrap_or(0)
        });
        trace_line!(
            "DXGI RotateResourceIdentities: rotated {} resources, alloc[0]=0x{:x}",
            n,
            first_handle,
        );
    }
    // HRESULT unchanged: every path returned 0 before and every path returns 0
    // now. Making a refused rotation FAIL the DDI is a separate decision with
    // its own blast radius.
    0
}

pub(crate) static ROTATE_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(crate) static BLT_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(crate) static BLT1_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(crate) static RESIDENCY_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(crate) static MPO_LOG_COUNT: LogThrottle = LogThrottle::new();
static MPO_TRACE: LogThrottle = LogThrottle::new();
static MPO_TRACE_DONE: LogThrottle = LogThrottle::new();
pub(crate) static PRESENT1_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(crate) static DXGI13_RESERVED_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(crate) const DXGI_MPO_MAX_PLANES: u32 = 1;

pub(crate) unsafe extern "C" fn dxgi_blt(arg: *mut ddi::DXGI_DDI_ARG_BLT) -> i32 {
    if arg.is_null() {
        return 0;
    }
    let a = &*arg;
    let Some(context) = d3d11_context(dxgi_device_handle(a.hDevice)) else {
        return 0;
    };
    let dst_h = dxgi_resource_handle(a.hDstResource);
    let src_h = dxgi_resource_handle(a.hSrcResource);
    let (Some(dst), Some(src)) = (load_resource(dst_h), load_resource(src_h)) else {
        log_error!(
            "DXGI Blt: missing resource dst=0x{:x} src=0x{:x}",
            a.hDstResource,
            a.hSrcResource
        );
        return 0;
    };

    if let Some(n) = BLT_LOG_COUNT.first_n_then_every_from_one(128, 512) {
        let mut src_desc = D3D11_TEXTURE2D_DESC::default();
        let mut dst_desc = D3D11_TEXTURE2D_DESC::default();
        let src_tex = (*src).cast::<ID3D11Texture2D>().ok();
        let dst_tex = (*dst).cast::<ID3D11Texture2D>().ok();
        if let Some(tex) = &src_tex {
            tex.GetDesc(&mut src_desc);
        }
        if let Some(tex) = &dst_tex {
            tex.GetDesc(&mut dst_desc);
        }
        trace_line!(
            "DXGI Blt: #{} src={:p}/{} alloc=0x{:x} {}x{} fmt={} -> \
             dst={:p}/{} alloc=0x{:x} {}x{} fmt={} flags=0x{:x} rotate={}",
            n,
            src_h.pDrvPrivate,
            a.SrcSubresource,
            resource_allocation(src_h),
            src_desc.Width,
            src_desc.Height,
            src_desc.Format.0,
            dst_h.pDrvPrivate,
            a.DstSubresource,
            resource_allocation(dst_h),
            dst_desc.Width,
            dst_desc.Height,
            dst_desc.Format.0,
            a.Flags.__bindgen_anon_1.Value,
            a.Rotate,
        );
    }

    // The DXGI 1.0 blit DDI has no source rectangle. For DWM/windowed present
    // setup the runtime uses it to move between compatible proxy/front-buffer
    // surfaces, so a full subresource copy is the safest baseline.
    context.CopySubresourceRegion(
        &*dst,
        a.DstSubresource,
        a.DstLeft,
        a.DstTop,
        0,
        &*src,
        a.SrcSubresource,
        None,
    );
    context.Flush();
    0
}

pub(crate) unsafe extern "C" fn dxgi_blt1(arg: *mut ddi::DXGI_DDI_ARG_BLT1) -> i32 {
    if arg.is_null() {
        return 0;
    }
    let a = &*arg;
    let Some(context) = d3d11_context(dxgi_device_handle(a.hDevice)) else {
        return 0;
    };
    let dst_h = dxgi_resource_handle(a.hDstResource);
    let src_h = dxgi_resource_handle(a.hSrcResource);
    let (Some(dst), Some(src)) = (load_resource(dst_h), load_resource(src_h)) else {
        log_error!(
            "DXGI Blt1: missing resource dst=0x{:x} src=0x{:x}",
            a.hDstResource,
            a.hSrcResource
        );
        return E_INVALIDARG;
    };

    const BLT_RESOLVE: u32 = 0x1;
    const BLT_CONVERT: u32 = 0x2;
    const BLT_STRETCH: u32 = 0x4;
    let flags = a.Flags.__bindgen_anon_1.Value;
    if flags & BLT_CONVERT != 0 {
        log_error!("DXGI Blt1: convert unsupported flags=0x{flags:x}");
        return DXGI_ERROR_UNSUPPORTED;
    }

    let src_w = a.SrcRight.saturating_sub(a.SrcLeft);
    let src_h_px = a.SrcBottom.saturating_sub(a.SrcTop);
    let dst_w = a.DstRight.saturating_sub(a.DstLeft);
    let dst_h_px = a.DstBottom.saturating_sub(a.DstTop);

    if flags & BLT_RESOLVE != 0 {
        let format = resource_dxgi_format(dst_h);
        if format.0 == 0 {
            log_error!("DXGI Blt1: resolve has unknown destination format");
            return E_INVALIDARG;
        }
        context.ResolveSubresource(&*dst, a.DstSubresource, &*src, a.SrcSubresource, format);
        context.Flush();
        return 0;
    }

    if flags & BLT_STRETCH != 0
        || (src_w != 0 && dst_w != 0 && (src_w != dst_w || src_h_px != dst_h_px))
    {
        log_error!(
            "DXGI Blt1: stretch unsupported src={}x{} dst={}x{} flags=0x{flags:x}",
            src_w,
            src_h_px,
            dst_w,
            dst_h_px
        );
        return DXGI_ERROR_UNSUPPORTED;
    }

    let bx;
    let bx_ptr = if a.SrcRight > a.SrcLeft && a.SrcBottom > a.SrcTop {
        bx = D3D11_BOX {
            left: a.SrcLeft,
            top: a.SrcTop,
            front: 0,
            right: a.SrcRight,
            bottom: a.SrcBottom,
            back: 1,
        };
        Some(&bx as *const D3D11_BOX)
    } else {
        None
    };

    if BLT1_LOG_COUNT.first_n(32).is_some() {
        trace_line!(
            "DXGI Blt1: copy src={}x{} dst={}x{} flags=0x{flags:x}",
            src_w,
            src_h_px,
            dst_w,
            dst_h_px
        );
    }

    context.CopySubresourceRegion(
        &*dst,
        a.DstSubresource,
        a.DstLeft,
        a.DstTop,
        0,
        &*src,
        a.SrcSubresource,
        bx_ptr,
    );
    context.Flush();
    0
}

pub(crate) unsafe extern "C" fn dxgi_offer_resources(
    arg: *mut ddi::DXGI_DDI_ARG_OFFERRESOURCES,
) -> i32 {
    if arg.is_null() {
        return 0;
    }
    let a = &*arg;
    if RESIDENCY_LOG_COUNT.first_n(32).is_some() {
        log_error!(
            "DXGI OfferResources: resources={} priority={} (kept resident)",
            a.Resources,
            a.Priority
        );
    }
    0
}

pub(crate) unsafe extern "C" fn dxgi_reclaim_resources(
    arg: *mut ddi::DXGI_DDI_ARG_RECLAIMRESOURCES,
) -> i32 {
    if arg.is_null() {
        return 0;
    }
    let a = &*arg;
    if !a.pDiscarded.is_null() {
        for i in 0..a.Resources as usize {
            *a.pDiscarded.add(i) = 0;
        }
    }
    if RESIDENCY_LOG_COUNT.first_n(32).is_some() {
        log_error!(
            "DXGI ReclaimResources: resources={} discarded=FALSE",
            a.Resources
        );
    }
    0
}

// D9 publishes one atomic UMD/KMD display profile. This is DWM's primary
// plane, not a claim of extra overlay hardware: one RGB plane, no scale,
// sharing, immediate flip, transform, stereo, YUV, or post-composition.
pub(crate) const RGB: u32 =
    ddi::DXGI_DDI_MULTIPLANE_OVERLAY_FEATURE_CAPS_DXGI_DDI_MULTIPLANE_OVERLAY_FEATURE_CAPS_RGB
        as u32;
pub(crate) const HELIOS_MPO_MAX_STRETCH: f32 = 1.0;
pub(crate) const HELIOS_MPO_MAX_SHRINK: f32 = 1.0;
pub(crate) const HELIOS_MPO_GROUPS: u32 = 1;
pub(crate) const HELIOS_MPO_OVERLAY_CAPS: u32 = RGB;

const MPO_PRESENT_FLIP_FLAG: u32 = 0x2;

fn same_mpo_rect(left: &ddi::RECT, right: &ddi::RECT) -> bool {
    left.left == right.left
        && left.top == right.top
        && left.right == right.right
        && left.bottom == right.bottom
}

fn full_resource_mpo_rect(rect: &ddi::RECT, width: u32, height: u32) -> bool {
    let (Ok(width), Ok(height)) = (i32::try_from(width), i32::try_from(height)) else {
        return false;
    };
    rect.left == 0
        && rect.top == 0
        && rect.right == width
        && rect.bottom == height
        && width != 0
        && height != 0
}

pub(crate) unsafe extern "C" fn dxgi_get_mpo_caps(
    arg: *mut ddi::DXGI_DDI_ARG_GETMULTIPLANEOVERLAYCAPS,
) -> i32 {
    if arg.is_null() || !arg.is_aligned() {
        return E_INVALIDARG;
    }
    let vidpn_source_id = core::ptr::addr_of!((*arg).VidPnSourceId).read();
    if vidpn_source_id != 0 {
        return E_INVALIDARG;
    }
    let caps = ddi::DXGI_DDI_MULTIPLANE_OVERLAY_CAPS {
        MaxPlanes: DXGI_MPO_MAX_PLANES,
        NumCapabilityGroups: HELIOS_MPO_GROUPS,
    };
    core::ptr::addr_of_mut!((*arg).MultiplaneOverlayCaps).write(caps);
    if MPO_LOG_COUNT.first_n(16).is_some() {
        log_error!(
            "DXGI GetMultiplaneOverlayCaps: MaxPlanes={} groups=1",
            DXGI_MPO_MAX_PLANES
        );
    }
    0
}

pub(crate) unsafe extern "C" fn dxgi_get_mpo_group_caps(
    arg: *mut ddi::DXGI_DDI_ARG_GETMULTIPLANEOVERLAYGROUPCAPS,
) -> i32 {
    if arg.is_null() || !arg.is_aligned() {
        return E_INVALIDARG;
    }
    let vidpn_source_id = core::ptr::addr_of!((*arg).VidPnSourceId).read();
    let group_index = core::ptr::addr_of!((*arg).GroupIndex).read();
    if vidpn_source_id != 0 || group_index != 0 {
        return E_INVALIDARG;
    }
    let caps = ddi::DXGI_DDI_MULTIPLANE_OVERLAY_GROUP_CAPS {
        NumPlanes: DXGI_MPO_MAX_PLANES,
        MaxStretchFactor: HELIOS_MPO_MAX_STRETCH,
        MaxShrinkFactor: HELIOS_MPO_MAX_SHRINK,
        OverlayCaps: HELIOS_MPO_OVERLAY_CAPS,
        StereoCaps: 0,
    };
    core::ptr::addr_of_mut!((*arg).MultiplaneOverlayGroupCaps).write(caps);
    if MPO_LOG_COUNT.first_n(16).is_some() {
        log_error!(
            "DXGI GetMultiplaneOverlayGroupCaps: group={} planes={} caps=0x{:x}",
            group_index,
            caps.NumPlanes,
            caps.OverlayCaps
        );
    }
    0
}

pub(crate) unsafe extern "C" fn dxgi_present_mpo(
    arg: *mut ddi::DXGI_DDI_ARG_PRESENTMULTIPLANEOVERLAY,
) -> i32 {
    probe_entry_attempt(PresentBoundaryEntry::Mpo);
    if arg.is_null() || !arg.is_aligned() {
        probe_early_refusal(PresentBoundaryEntry::Mpo, "null or misaligned MPO argument");
        return E_INVALIDARG;
    }
    let a = core::ptr::read(arg);
    if a.PresentPlaneCount != DXGI_MPO_MAX_PLANES {
        probe_early_refusal(
            PresentBoundaryEntry::Mpo,
            "MPO plane count is not exactly one",
        );
        log_error!(
            "DXGI PresentMultiplaneOverlay: expected one plane, got {}",
            a.PresentPlaneCount
        );
        return DXGI_ERROR_UNSUPPORTED;
    }
    if a.pPresentPlanes.is_null() || !a.pPresentPlanes.is_aligned() {
        probe_early_refusal(PresentBoundaryEntry::Mpo, "null or misaligned MPO plane");
        return E_INVALIDARG;
    }
    if a.Reserved != 0 {
        probe_early_refusal(PresentBoundaryEntry::Mpo, "nonzero MPO reserved field");
        return E_INVALIDARG;
    }
    if a.VidPnSourceId != 0 {
        probe_early_refusal(PresentBoundaryEntry::Mpo, "MPO source is not source zero");
        return DXGI_ERROR_UNSUPPORTED;
    }
    let present_flags =
        core::ptr::read((&a.Flags as *const ddi::DXGI_DDI_PRESENT_FLAGS).cast::<u32>());
    if present_flags != MPO_PRESENT_FLIP_FLAG {
        probe_early_refusal(PresentBoundaryEntry::Mpo, "unsupported MPO present flags");
        return DXGI_ERROR_UNSUPPORTED;
    }
    if !(ddi::DXGI_DDI_FLIP_INTERVAL_TYPE_DXGI_DDI_FLIP_INTERVAL_ONE
        ..=ddi::DXGI_DDI_FLIP_INTERVAL_TYPE_DXGI_DDI_FLIP_INTERVAL_FOUR)
        .contains(&a.FlipInterval)
    {
        probe_early_refusal(
            PresentBoundaryEntry::Mpo,
            "immediate or invalid MPO interval",
        );
        return DXGI_ERROR_UNSUPPORTED;
    }

    let plane = core::ptr::read(a.pPresentPlanes);
    let attrs = plane.PlaneAttributes;
    if plane.LayerIndex != 0 || plane.Enabled != 1 || plane.hResource == 0 {
        probe_early_refusal(
            PresentBoundaryEntry::Mpo,
            "MPO plane is not enabled layer zero",
        );
        return DXGI_ERROR_UNSUPPORTED;
    }
    if plane.SubResourceIndex != 0
        || attrs.Flags != 0
        || attrs.Rotation
            != ddi::DXGI_DDI_MODE_ROTATION_DXGI_DDI_MODE_ROTATION_IDENTITY
        || attrs.Blend
            != ddi::DXGI_DDI_MULTIPLANE_OVERLAY_BLEND_DXGI_DDI_MULTIPLANE_OVERLAY_BLEND_OPAQUE
        || attrs.VideoFrameFormat
            != ddi::DXGI_DDI_MULTIPLANE_OVERLAY_VIDEO_FRAME_FORMAT_DXGI_DDI_MULIIPLANE_OVERLAY_VIDEO_FRAME_FORMAT_PROGRESSIVE
        || attrs.YCbCrFlags != 0
        || attrs.StereoFormat
            != ddi::DXGI_DDI_MULTIPLANE_OVERLAY_STEREO_FORMAT_DXGI_DDI_MULTIPLANE_OVERLAY_STEREO_FORMAT_MONO
        || attrs.StereoLeftViewFrame0 != 0
        || attrs.StereoBaseViewFrame0 != 0
        || attrs.StereoFlipMode
            != ddi::DXGI_DDI_MULTIPLANE_OVERLAY_STEREO_FLIP_MODE_DXGI_DDI_MULTIPLANE_OVERLAY_STEREO_FLIP_NONE
        // ⛔ `StretchQuality` is NOT checked, and must not be. This gate already
        // requires SrcRect == DstRect == ClipRect below, so the plane is never
        // scaled and the filter the field names cannot affect a single pixel.
        // Requiring 0 refused DWM's very first MPO present on every boot
        // (measured 2026-08-24: `failing=[stretch_quality] ... stretch=1
        // src=(0,0)-(1280,800) dst=(0,0)-(1280,800) clip=(0,0)-(1280,800)`),
        // and DWM answered by destroying the device without presenting — the
        // black desktop. 1 is `..._STRETCH_QUALITY_BILINEAR`; 0 is not even a
        // defined enumerator, so the old test could only ever pass by accident.
        || !same_mpo_rect(&attrs.SrcRect, &attrs.DstRect)
        || !same_mpo_rect(&attrs.SrcRect, &attrs.ClipRect)
    {
        // ⛔ ONE message for thirteen predicates was unattributable: DWM's very
        // first MPO present lands here and the device is torn down, so the
        // refusal costs a whole boot per guess. Name every failing field and
        // print the values with it.
        let failing: [(&str, bool); 12] = [
            ("subresource", plane.SubResourceIndex != 0),
            ("attr_flags", attrs.Flags != 0),
            (
                "rotation",
                attrs.Rotation != ddi::DXGI_DDI_MODE_ROTATION_DXGI_DDI_MODE_ROTATION_IDENTITY,
            ),
            (
                "blend",
                attrs.Blend
                    != ddi::DXGI_DDI_MULTIPLANE_OVERLAY_BLEND_DXGI_DDI_MULTIPLANE_OVERLAY_BLEND_OPAQUE,
            ),
            (
                "frame_format",
                attrs.VideoFrameFormat
                    != ddi::DXGI_DDI_MULTIPLANE_OVERLAY_VIDEO_FRAME_FORMAT_DXGI_DDI_MULIIPLANE_OVERLAY_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            ),
            ("ycbcr_flags", attrs.YCbCrFlags != 0),
            (
                "stereo_format",
                attrs.StereoFormat
                    != ddi::DXGI_DDI_MULTIPLANE_OVERLAY_STEREO_FORMAT_DXGI_DDI_MULTIPLANE_OVERLAY_STEREO_FORMAT_MONO,
            ),
            ("stereo_left_view", attrs.StereoLeftViewFrame0 != 0),
            ("stereo_base_view", attrs.StereoBaseViewFrame0 != 0),
            (
                "stereo_flip",
                attrs.StereoFlipMode
                    != ddi::DXGI_DDI_MULTIPLANE_OVERLAY_STEREO_FLIP_MODE_DXGI_DDI_MULTIPLANE_OVERLAY_STEREO_FLIP_NONE,
            ),
            ("src_vs_dst", !same_mpo_rect(&attrs.SrcRect, &attrs.DstRect)),
            ("src_vs_clip", !same_mpo_rect(&attrs.SrcRect, &attrs.ClipRect)),
        ];
        let mut names = [""; 12];
        let mut n = 0;
        for (name, bad) in failing {
            if bad {
                names[n] = name;
                n += 1;
            }
        }
        log_error!(
            "DXGI PresentMultiplaneOverlay REFUSED: failing=[{}] sub={} flags=0x{:x} rot={} \
             blend={} frame={} ycbcr=0x{:x} stereo_fmt={} stereo_l={} stereo_b={} stereo_flip={} \
             stretch={} src=({},{})-({},{}) dst=({},{})-({},{}) clip=({},{})-({},{})",
            names[..n].join(","),
            plane.SubResourceIndex,
            attrs.Flags,
            attrs.Rotation,
            attrs.Blend,
            attrs.VideoFrameFormat,
            attrs.YCbCrFlags,
            attrs.StereoFormat,
            attrs.StereoLeftViewFrame0,
            attrs.StereoBaseViewFrame0,
            attrs.StereoFlipMode,
            attrs.StretchQuality,
            attrs.SrcRect.left,
            attrs.SrcRect.top,
            attrs.SrcRect.right,
            attrs.SrcRect.bottom,
            attrs.DstRect.left,
            attrs.DstRect.top,
            attrs.DstRect.right,
            attrs.DstRect.bottom,
            attrs.ClipRect.left,
            attrs.ClipRect.top,
            attrs.ClipRect.right,
            attrs.ClipRect.bottom,
        );
        probe_early_refusal(PresentBoundaryEntry::Mpo, "unsupported MPO plane attributes");
        return DXGI_ERROR_UNSUPPORTED;
    }

    let resource = dxgi_resource_handle(plane.hResource);
    let Some(resource_object) = load_resource(resource) else {
        probe_early_refusal(PresentBoundaryEntry::Mpo, "MPO resource is not live");
        return E_INVALIDARG;
    };
    let Ok(texture) = (*resource_object).cast::<ID3D11Texture2D>() else {
        probe_early_refusal(
            PresentBoundaryEntry::Mpo,
            "MPO resource is not a 2D texture",
        );
        return DXGI_ERROR_UNSUPPORTED;
    };
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    texture.GetDesc(&mut desc);
    if desc.MipLevels != 1
        || desc.ArraySize != 1
        || desc.SampleDesc.Count != 1
        || desc.SampleDesc.Quality != 0
        || desc.Format != windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM
        || !full_resource_mpo_rect(&attrs.SrcRect, desc.Width, desc.Height)
    {
        probe_early_refusal(
            PresentBoundaryEntry::Mpo,
            "MPO resource profile is unsupported",
        );
        log_error!(
            "DXGI PresentMultiplaneOverlay: unsupported resource {}x{} fmt={} mips={} array={} sample={}x{}",
            desc.Width,
            desc.Height,
            desc.Format.0,
            desc.MipLevels,
            desc.ArraySize,
            desc.SampleDesc.Count,
            desc.SampleDesc.Quality
        );
        return DXGI_ERROR_UNSUPPORTED;
    }

    let h = dxgi_device_handle(a.hDevice);
    let Some(dev) = helios_device(h) else {
        probe_early_refusal(PresentBoundaryEntry::Mpo, "MPO device handle not live");
        return E_INVALIDARG;
    };
    let (false, Some(ctx)) = (dev.dxgi_callbacks.is_null(), dev.outer.context.as_ref()) else {
        if dev.dxgi_callbacks.is_null() {
            PRESENT_BOUNDARY_MPO_MISSING.fetch_add(1, Ordering::Relaxed);
        }
        probe_early_refusal(
            PresentBoundaryEntry::Mpo,
            "MPO callback table or context missing",
        );
        log_error!("DXGI PresentMultiplaneOverlay: no DXGI callbacks/context");
        return DXGI_ERROR_UNSUPPORTED;
    };
    let Some(present_cb) = (*dev.dxgi_callbacks).pfnPresentMultiplaneOverlayCb else {
        PRESENT_BOUNDARY_MPO_MISSING.fetch_add(1, Ordering::Relaxed);
        probe_early_refusal(
            PresentBoundaryEntry::Mpo,
            "pfnPresentMultiplaneOverlayCb missing",
        );
        log_error!("DXGI PresentMultiplaneOverlay: pfnPresentMultiplaneOverlayCb missing");
        return DXGI_ERROR_UNSUPPORTED;
    };

    let mut cb = ddi::DXGIDDICB_PRESENT_MULTIPLANE_OVERLAY::default();
    cb.pDXGIContext = a.pDXGIContext;
    cb.hContext = ctx.handle.as_ptr();
    cb.BroadcastContextCount = 0;
    let alloc = resource_allocation(resource);
    if alloc == 0 {
        probe_early_refusal(
            PresentBoundaryEntry::Mpo,
            "MPO plane source allocation missing",
        );
        log_error!(
            "DXGI PresentMultiplaneOverlay: layer zero has no allocation hResource=0x{:x}",
            plane.hResource
        );
        return E_INVALIDARG;
    }
    cb.AllocationInfo[0].PresentAllocation = alloc;
    cb.AllocationInfo[0].SubResourceIndex = 0;
    cb.AllocationInfoCount = 1;

    if MPO_LOG_COUNT.first_n(128).is_some() {
        trace_line!(
            "DXGI MPO primary: hRes=0x{:x} allocation=0x{:x} {}x{} flags=0x{:x} interval={} dirty={}",
            plane.hResource,
            alloc,
            desc.Width,
            desc.Height,
            present_flags,
            a.FlipInterval,
            attrs.DirtyRectCount
        );
    }

    probe_mpo_entry(&a, &cb);

    // Same submission synchronization as finish_present: DXVK's Flush() is
    // asynchronous, and the flip must queue behind this device's own render.
    // (dwm's cross-device case is ordered by pfnFlush — transfer.rs `flush`.)
    if MPO_TRACE.first_n(96).is_some() {
        log_error!(
            "DDI PresentMPO t={} hContext={:p} srcAlloc=0x{:x}",
            crate::forward::trace_us(),
            ctx.handle.as_ptr(),
            alloc
        );
    }
    if !dev.dxvk.flush_submitted() {
        probe_early_refusal(
            PresentBoundaryEntry::Mpo,
            "DXVK submission synchronization failed",
        );
        log_error!("DXGI PresentMultiplaneOverlay: DXVK submission synchronization failed");
        return E_FAIL;
    }

    let hr = present_cb(dev.h_rt_device, &cb);
    probe_mpo_result(hr, &cb);
    if MPO_TRACE_DONE.first_n(96).is_some() {
        log_error!(
            "DDI PresentMPO done t={} hr=0x{:08x}",
            crate::forward::trace_us(),
            hr as u32
        );
    }
    if MPO_LOG_COUNT.first_n(64).is_some() {
        trace_line!(
            "DXGI PresentMultiplaneOverlay: planes={} enabled={} presentCb=0x{:08x} ctx={:p}",
            a.PresentPlaneCount,
            cb.AllocationInfoCount,
            hr as u32,
            ctx.handle.as_ptr()
        );
    }
    hr
}

pub(crate) unsafe extern "C" fn dxgi_reserved_unsupported(_arg: *mut c_void) -> i32 {
    if DXGI13_RESERVED_LOG_COUNT.first_n(16).is_some() {
        log_error!("DXGI reserved callback -> DXGI_ERROR_UNSUPPORTED");
    }
    DXGI_ERROR_UNSUPPORTED
}

pub(crate) unsafe extern "C" fn dxgi_present1(arg: *mut ddi::DXGI_DDI_ARG_PRESENT1) -> i32 {
    if arg.is_null() {
        probe_early_refusal(
            PresentBoundaryEntry::Present1Multi,
            "null Present1 argument",
        );
        return E_INVALIDARG;
    }
    let a = &*arg;
    if a.SurfacesToPresent == 0 || a.phSurfacesToPresent.is_null() {
        probe_early_refusal(
            PresentBoundaryEntry::Present1Multi,
            "Present1 has no source surfaces",
        );
        log_error!("DXGI Present1: no source surfaces");
        return E_INVALIDARG;
    }

    if a.SurfacesToPresent == 1 {
        probe_entry_attempt(PresentBoundaryEntry::Present1Single);
        let source = *a.phSurfacesToPresent;
        let mut present = ddi::DXGI_DDI_ARG_PRESENT {
            hDevice: a.hDevice,
            hSurfaceToPresent: source.hSurface,
            SrcSubResourceIndex: source.SubResourceIndex,
            hDstResource: a.hDstResource,
            DstSubResourceIndex: a.DstSubResourceIndex,
            pDXGIContext: a.pDXGIContext,
            Flags: a.Flags,
            FlipInterval: a.FlipInterval,
        };
        return dxgi_present_impl(&mut present, PresentBoundaryEntry::Present1Single);
    }

    // WDDM 1.3 Present1's surface array is not an old single-source Present.
    // Earlier entries are part of the DXGI display/release list; the documented
    // callback contract for a many-resource present is specifically to translate
    // only the last source handle into DXGIDDICB_PRESENT. Dirty rects are hints
    // and must never be a failure reason.
    let source_index = a.SurfacesToPresent as usize - 1;
    let source = *a.phSurfacesToPresent.add(source_index);
    let h = dxgi_device_handle(a.hDevice);
    let src_h = dxgi_resource_handle(source.hSurface);
    let dst_h = dxgi_resource_handle(a.hDstResource);
    let src_alloc = resource_allocation(src_h);
    let dst_alloc = resource_allocation(dst_h);
    probe_entry_attempt(PresentBoundaryEntry::Present1Multi);
    probe_present1_multi_entry(
        a,
        source_index,
        source.hSurface as usize,
        source.SubResourceIndex,
        src_alloc,
        dst_alloc,
    );
    if PRESENT1_LOG_COUNT.first_n(64).is_some() {
        trace_line!(
            "DXGI Present1 multi: surfaces={} callback_src={} src={:p}/{} alloc=0x{:x} \
             dst={:p}/{} dstAlloc=0x{:x} dirty={} multiplicity={} flags=0x{:x}",
            a.SurfacesToPresent,
            source_index,
            source.hSurface as *mut c_void,
            source.SubResourceIndex,
            src_alloc,
            a.hDstResource as *mut c_void,
            a.DstSubResourceIndex,
            dst_alloc,
            a.DirtyRects,
            a.BackBufferMultiplicity,
            *(&a.Flags as *const ddi::DXGI_DDI_PRESENT_FLAGS as *const u32),
        );
    }

    if src_alloc == 0 {
        probe_early_refusal(
            PresentBoundaryEntry::Present1Multi,
            "Present1 callback source allocation missing",
        );
        log_error!(
            "DXGI Present1 multi: callback source has no allocation hResource=0x{:x}",
            source.hSurface
        );
        return E_INVALIDARG;
    }

    if let Some(context) = d3d11_context(h) {
        context.Flush();
    }

    let present_hr = match finish_present(
        h,
        src_h,
        dst_h,
        src_alloc,
        dst_alloc,
        PresentRequest {
            kind: PresentKind::Present1Multi,
            boundary: PresentBoundaryEntry::Present1Multi,
            dxgi_context: a.pDXGIContext,
            flags: *(&a.Flags as *const ddi::DXGI_DDI_PRESENT_FLAGS as *const u32),
        },
    ) {
        Ok(hr) => hr,
        // Skips the trailing PRESENT1_LOG_COUNT line, exactly as the bare
        // `return DXGI_ERROR_UNSUPPORTED` / `return E_FAIL` did.
        Err(hr) => return hr,
    };

    if PRESENT1_LOG_COUNT.first_n(64).is_some() {
        trace_line!(
            "DXGI Present1 multi: presentCb=0x{:08x} srcAlloc=0x{:x} dstAlloc=0x{:x} opt_comp={} \
             dxgiCtx={:p} hContext={:p}",
            present_hr as u32,
            src_alloc,
            dst_alloc,
            present_optimize_composition_enabled() as u32,
            a.pDXGIContext,
            dev_context_for_log(h)
        );
    }
    present_hr
}

pub(crate) unsafe extern "C" fn dxgi_check_present_duration_support(
    arg: *mut ddi::DXGI_DDI_ARG_CHECKPRESENTDURATIONSUPPORT,
) -> i32 {
    if arg.is_null() {
        return 0;
    }
    let a = &mut *arg;
    a.ClosestSmallerDuration = 0;
    a.ClosestLargerDuration = 0;
    if PRESENT1_LOG_COUNT.first_n(16).is_some() {
        log_error!(
            "DXGI CheckPresentDurationSupport: desired={} smaller=0 larger=0",
            a.DesiredPresentDuration
        );
    }
    0
}
