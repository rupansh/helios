//! Pure direct-scanout admission over flattened values; success proves only
//! compatibility and grants neither retention nor plane ownership. It models
//! the retirement target; current OPAQUE/platform mismatches gate activation.

use helios_protocol::{
    helios_hwa2_swizzle_is_scanout_bindable, HeliosAdapterMatch, HeliosAllocDescRejection,
    HeliosWddmAllocationDescV2, D3DDDIFMT_A8R8G8B8, D3DDDI_ID_UNINITIALIZED,
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_B8G8R8X8_UNORM, HELIOS_HWA2_FLAG_CROSS_ADAPTER,
    HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY, HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE,
    HELIOS_HWA2_FLAG_DISPLAYABLE, HELIOS_HWA2_FLAG_PRIMARY, HELIOS_HWA2_FLAG_PROTECTED,
    HELIOS_HWA2_FLAG_STANDARD, HELIOS_HWA2_FLAG_STEREO, HELIOS_HWA2_KIND_IMAGE,
    HELIOS_HWA2_KIND_STANDARD_PRIMARY, HELIOS_HWA2_SWIZZLE_LINEAR, HELIOS_PACKAGE_GENERATION,
};

const SHARED_PRIMARY_STANDARD_ALLOCATION_TYPE: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommittedMode {
    pub generation: u64,
    pub source_id: u32,
    pub target_id: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub target_width: u32,
    pub target_height: u32,
    pub active: bool,
    pub visible: bool,
    pub powered: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperationFacts {
    pub current_mode_generation: u64,
    pub adapter_match: HeliosAdapterMatch,
    pub immediate_flip: bool,
    pub stereo: bool,
    pub shared_primary_transition: bool,
    pub independent_flip_exclusive: bool,
    /// The classic `ModeChange` programming flip. Measured on 26100.8972
    /// (S-ring, KMD 22.22.328.0): dxgkrnl orders CommitVidPn →
    /// SetVidPnSourceAddress(MODE_CHANGE) → SetVidPnSourceVisibility(TRUE), so
    /// this one flip legally arrives while the source is still invisible.
    pub mode_change: bool,
    pub unsupported_or_reserved_flags: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlaneRect {
    pub left: i64,
    pub top: i64,
    pub right: i64,
    pub bottom: i64,
}

impl PlaneRect {
    pub const fn full_output(width: u32, height: u32) -> Self {
        Self {
            left: 0,
            top: 0,
            right: width as i64,
            bottom: height as i64,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MpoPlaneFacts {
    pub plane_count: u32,
    pub layer_index: u32,
    pub source_rect: PlaneRect,
    pub destination_rect: PlaneRect,
    pub clip_rect: PlaneRect,
    pub identity_rotation: bool,
    pub vertical_flip: bool,
    pub horizontal_flip: bool,
    pub alpha_blend: bool,
    pub sdr_rgb: bool,
    pub scaling: bool,
    pub post_composition: bool,
    pub hdr_metadata: bool,
    pub unsupported_or_reserved_attributes_or_features: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MpoSetPlaneFacts {
    pub plane: MpoPlaneFacts,
    pub enabled: bool,
    pub context_count: u32,
    pub context_record_present: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaneFacts {
    Classic,
    MpoCheck(MpoPlaneFacts),
    MpoSet(MpoSetPlaneFacts),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refusal {
    InvalidFinalHwa2(HeliosAllocDescRejection),
    CommittedModeGenerationZero,
    CurrentModeGenerationZero,
    StaleCommittedModeGeneration {
        committed: u64,
        current: u64,
    },
    CommittedModeInactive,
    SourceInvisible,
    SourcePoweredOff,
    OsSourceUninitialized,
    CommittedSourceUninitialized,
    CommittedTargetUninitialized,
    CommittedSourceMismatch {
        committed: u32,
        os: u32,
    },
    AdapterDifferentOrUnknown,
    ImmediateFlipRequested,
    StereoOperationRequested,
    UnsupportedOrReservedOperationFlags {
        found: u32,
    },
    AllocationKindNotImageOrStandardPrimary {
        found: u32,
    },
    OrdinaryImageHasStandardSemantics {
        flags: u32,
        standard_allocation_type: u32,
    },
    StandardPrimarySemanticsMismatch {
        flags: u32,
        standard_allocation_type: u32,
    },
    PrimaryFlagMissing,
    DisplayableFlagMissing,
    DirectFlipCompatibleFlagMissing,
    StereoAllocation,
    ProtectedAllocation,
    CrossAdapterAllocation,
    FormatNotBgra8 {
        found: u32,
    },
    D3dDdiFormatNotA8R8G8B8 {
        found: u32,
    },
    CommittedSourceExtentZero,
    CommittedTargetExtentZero,
    CommittedSourceTargetExtentMismatch {
        source_width: u32,
        source_height: u32,
        target_width: u32,
        target_height: u32,
    },
    AllocationSourceExtentMismatch {
        allocation_width: u32,
        allocation_height: u32,
        source_width: u32,
        source_height: u32,
    },
    DepthOrArraySizeNotOne {
        found: u32,
    },
    MipLevelsNotOne {
        found: u32,
    },
    SampleCountNotOne {
        found: u32,
    },
    SampleQualityNotZero {
        found: u32,
    },
    AllocationPlaneCountNotOne {
        found: u32,
    },
    PlaneRowPitchTooSmall {
        found: u32,
        minimum: u64,
    },
    PlaneRowPitchNotPixelAligned {
        found: u32,
    },
    PlaneFullFrameArithmeticOverflow {
        offset: u64,
        row_pitch: u32,
        height: u32,
        minimum_row_pitch: u64,
    },
    PlaneFullFrameRangeExceedsBacking {
        end: u64,
        byte_size: u64,
    },
    PlaneSlicePitchTooSmall {
        found: u32,
        minimum: u64,
    },
    PlaneOffsetExceedsSetScanoutBlob {
        found: u64,
    },
    UnsupportedSwizzleClass {
        found: u32,
    },
    AllocationSourceMismatch {
        allocation: u32,
        os: u32,
    },
    MpoPlaneCountNotOne {
        found: u32,
    },
    MpoLayerNotZero {
        found: u32,
    },
    MpoUnsupportedOrReservedAttributesOrFeatures {
        found: u64,
    },
    MpoSetEnabledInputFlagMissing,
    MpoContextCountNotOne {
        found: u32,
    },
    MpoContextRecordMissing,
    MpoSourceRectNotFullOutput,
    MpoDestinationRectNotFullOutput,
    MpoClipRectNotFullOutput,
    MpoRotationNotIdentity,
    MpoVerticalFlip,
    MpoHorizontalFlip,
    MpoAlphaBlend,
    MpoColorSpaceNotSdrRgb,
    MpoScaling,
    MpoPostComposition,
    MpoHdrMetadata,
}

/// The refusal's diagnostic code and its most informative scalar.
///
/// `code` is the variant's 1-based declaration index + 63, which reproduces the
/// fourteen codes already published in `D2AdmWhy` (0x45 `SourceInvisible` ..
/// 0x6A `AllocationSourceMismatch`) and extends the scheme to every arm. The
/// range is 0x40..=0x7B, so it stays clear of the 0x7F "unnamed" bucket this
/// replaces -- a bucket that hid 32 real refusals on 22.22.341.0.
pub fn refusal_code_and_detail(refusal: Refusal) -> (u32, u32) {
    use Refusal as R;
    match refusal {
        R::InvalidFinalHwa2(..) => (0x40, 0),
        R::CommittedModeGenerationZero => (0x41, 0),
        R::CurrentModeGenerationZero => (0x42, 0),
        R::StaleCommittedModeGeneration { current, .. } => (0x43, current as u32),
        R::CommittedModeInactive => (0x44, 0),
        R::SourceInvisible => (0x45, 0),
        R::SourcePoweredOff => (0x46, 0),
        R::OsSourceUninitialized => (0x47, 0),
        R::CommittedSourceUninitialized => (0x48, 0),
        R::CommittedTargetUninitialized => (0x49, 0),
        R::CommittedSourceMismatch { os, .. } => (0x4A, os),
        R::AdapterDifferentOrUnknown => (0x4B, 0),
        R::ImmediateFlipRequested => (0x4C, 0),
        R::StereoOperationRequested => (0x4D, 0),
        R::UnsupportedOrReservedOperationFlags { found, .. } => (0x4E, found),
        R::AllocationKindNotImageOrStandardPrimary { found, .. } => (0x4F, found),
        R::OrdinaryImageHasStandardSemantics { flags, .. } => (0x50, flags),
        R::StandardPrimarySemanticsMismatch { flags, .. } => (0x51, flags),
        R::PrimaryFlagMissing => (0x52, 0),
        R::DisplayableFlagMissing => (0x53, 0),
        R::DirectFlipCompatibleFlagMissing => (0x54, 0),
        R::StereoAllocation => (0x55, 0),
        R::ProtectedAllocation => (0x56, 0),
        R::CrossAdapterAllocation => (0x57, 0),
        R::FormatNotBgra8 { found, .. } => (0x58, found),
        R::D3dDdiFormatNotA8R8G8B8 { found, .. } => (0x59, found),
        R::CommittedSourceExtentZero => (0x5A, 0),
        R::CommittedTargetExtentZero => (0x5B, 0),
        R::CommittedSourceTargetExtentMismatch { source_height, source_width, .. } => (0x5C, ((source_width & 0xffff) << 16) | (source_height & 0xffff)),
        R::AllocationSourceExtentMismatch { allocation_height, allocation_width, .. } => (0x5D, ((allocation_width & 0xffff) << 16) | (allocation_height & 0xffff)),
        R::DepthOrArraySizeNotOne { found, .. } => (0x5E, found),
        R::MipLevelsNotOne { found, .. } => (0x5F, found),
        R::SampleCountNotOne { found, .. } => (0x60, found),
        R::SampleQualityNotZero { found, .. } => (0x61, found),
        R::AllocationPlaneCountNotOne { found, .. } => (0x62, found),
        R::PlaneRowPitchTooSmall { found, .. } => (0x63, found),
        R::PlaneRowPitchNotPixelAligned { found, .. } => (0x64, found),
        R::PlaneFullFrameArithmeticOverflow { row_pitch, .. } => (0x65, row_pitch),
        R::PlaneFullFrameRangeExceedsBacking { end, .. } => (0x66, end as u32),
        R::PlaneSlicePitchTooSmall { found, .. } => (0x67, found),
        R::PlaneOffsetExceedsSetScanoutBlob { found, .. } => (0x68, found as u32),
        R::UnsupportedSwizzleClass { found, .. } => (0x69, found),
        R::AllocationSourceMismatch { allocation, .. } => (0x6A, allocation),
        R::MpoPlaneCountNotOne { found, .. } => (0x6B, found),
        R::MpoLayerNotZero { found, .. } => (0x6C, found),
        R::MpoUnsupportedOrReservedAttributesOrFeatures { found, .. } => (0x6D, found as u32),
        R::MpoSetEnabledInputFlagMissing => (0x6E, 0),
        R::MpoContextCountNotOne { found, .. } => (0x6F, found),
        R::MpoContextRecordMissing => (0x70, 0),
        R::MpoSourceRectNotFullOutput => (0x71, 0),
        R::MpoDestinationRectNotFullOutput => (0x72, 0),
        R::MpoClipRectNotFullOutput => (0x73, 0),
        R::MpoRotationNotIdentity => (0x74, 0),
        R::MpoVerticalFlip => (0x75, 0),
        R::MpoHorizontalFlip => (0x76, 0),
        R::MpoAlphaBlend => (0x77, 0),
        R::MpoColorSpaceNotSdrRgb => (0x78, 0),
        R::MpoScaling => (0x79, 0),
        R::MpoPostComposition => (0x7A, 0),
        R::MpoHdrMetadata => (0x7B, 0),
    }
}

pub fn validate_direct_scanout_binding(
    allocation: &HeliosWddmAllocationDescV2,
    mode: &CommittedMode,
    os_source: u32,
    operation: &OperationFacts,
    plane: &PlaneFacts,
) -> Result<(), Refusal> {
    allocation
        .validate_create_output(HELIOS_PACKAGE_GENERATION)
        .map_err(Refusal::InvalidFinalHwa2)?;

    if mode.generation == 0 {
        return Err(Refusal::CommittedModeGenerationZero);
    }
    if operation.current_mode_generation == 0 {
        return Err(Refusal::CurrentModeGenerationZero);
    }
    if mode.generation != operation.current_mode_generation {
        return Err(Refusal::StaleCommittedModeGeneration {
            committed: mode.generation,
            current: operation.current_mode_generation,
        });
    }
    if !mode.active {
        return Err(Refusal::CommittedModeInactive);
    }
    if !mode.visible && !operation.mode_change {
        return Err(Refusal::SourceInvisible);
    }
    if !mode.powered {
        return Err(Refusal::SourcePoweredOff);
    }
    if os_source == D3DDDI_ID_UNINITIALIZED {
        return Err(Refusal::OsSourceUninitialized);
    }
    if mode.source_id == D3DDDI_ID_UNINITIALIZED {
        return Err(Refusal::CommittedSourceUninitialized);
    }
    if mode.target_id == D3DDDI_ID_UNINITIALIZED {
        return Err(Refusal::CommittedTargetUninitialized);
    }
    if mode.source_id != os_source {
        return Err(Refusal::CommittedSourceMismatch {
            committed: mode.source_id,
            os: os_source,
        });
    }
    if operation.adapter_match != HeliosAdapterMatch::SameAdapter {
        return Err(Refusal::AdapterDifferentOrUnknown);
    }
    if operation.immediate_flip {
        return Err(Refusal::ImmediateFlipRequested);
    }
    if operation.stereo {
        return Err(Refusal::StereoOperationRequested);
    }
    if operation.unsupported_or_reserved_flags != 0 {
        return Err(Refusal::UnsupportedOrReservedOperationFlags {
            found: operation.unsupported_or_reserved_flags,
        });
    }

    match allocation.allocation_kind {
        HELIOS_HWA2_KIND_IMAGE => {
            if allocation.flags & HELIOS_HWA2_FLAG_STANDARD != 0
                || allocation.standard_allocation_type != 0
            {
                return Err(Refusal::OrdinaryImageHasStandardSemantics {
                    flags: allocation.flags,
                    standard_allocation_type: allocation.standard_allocation_type,
                });
            }
        }
        HELIOS_HWA2_KIND_STANDARD_PRIMARY => {
            if allocation.flags & HELIOS_HWA2_FLAG_STANDARD == 0
                || allocation.standard_allocation_type != SHARED_PRIMARY_STANDARD_ALLOCATION_TYPE
            {
                return Err(Refusal::StandardPrimarySemanticsMismatch {
                    flags: allocation.flags,
                    standard_allocation_type: allocation.standard_allocation_type,
                });
            }
        }
        found => return Err(Refusal::AllocationKindNotImageOrStandardPrimary { found }),
    }

    if allocation.flags & HELIOS_HWA2_FLAG_PRIMARY == 0 {
        return Err(Refusal::PrimaryFlagMissing);
    }
    if allocation.flags & HELIOS_HWA2_FLAG_DISPLAYABLE == 0 {
        return Err(Refusal::DisplayableFlagMissing);
    }
    if allocation.flags & HELIOS_HWA2_FLAG_STEREO != 0 {
        return Err(Refusal::StereoAllocation);
    }
    if allocation.flags & HELIOS_HWA2_FLAG_PROTECTED != 0 {
        return Err(Refusal::ProtectedAllocation);
    }
    if allocation.flags & HELIOS_HWA2_FLAG_CROSS_ADAPTER != 0 {
        return Err(Refusal::CrossAdapterAllocation);
    }
    // 88 (B8G8R8X8) is what the KMD's own standard-primary author writes — XR24
    // is the measured egl-headless scanout format (39th session) — and 87 stays
    // admitted for UMD-authored primaries. Demanding 87 alone refused every OS
    // shared-primary flip with D2AdmWhy=4 (measured, KMD 22.22.330.0).
    if allocation.dxgi_format != DXGI_FORMAT_B8G8R8A8_UNORM
        && allocation.dxgi_format != DXGI_FORMAT_B8G8R8X8_UNORM
    {
        return Err(Refusal::FormatNotBgra8 {
            found: allocation.dxgi_format,
        });
    }
    if allocation.d3d_ddi_format != D3DDDIFMT_A8R8G8B8 {
        return Err(Refusal::D3dDdiFormatNotA8R8G8B8 {
            found: allocation.d3d_ddi_format,
        });
    }
    if mode.source_width == 0 || mode.source_height == 0 {
        return Err(Refusal::CommittedSourceExtentZero);
    }
    if mode.target_width == 0 || mode.target_height == 0 {
        return Err(Refusal::CommittedTargetExtentZero);
    }
    if mode.source_width != mode.target_width || mode.source_height != mode.target_height {
        return Err(Refusal::CommittedSourceTargetExtentMismatch {
            source_width: mode.source_width,
            source_height: mode.source_height,
            target_width: mode.target_width,
            target_height: mode.target_height,
        });
    }
    if allocation.width != mode.source_width || allocation.height != mode.source_height {
        return Err(Refusal::AllocationSourceExtentMismatch {
            allocation_width: allocation.width,
            allocation_height: allocation.height,
            source_width: mode.source_width,
            source_height: mode.source_height,
        });
    }
    if allocation.depth_or_array_size != 1 {
        return Err(Refusal::DepthOrArraySizeNotOne {
            found: allocation.depth_or_array_size,
        });
    }
    if allocation.mip_levels != 1 {
        return Err(Refusal::MipLevelsNotOne {
            found: allocation.mip_levels,
        });
    }
    if allocation.sample_count != 1 {
        return Err(Refusal::SampleCountNotOne {
            found: allocation.sample_count,
        });
    }
    if allocation.sample_quality != 0 {
        return Err(Refusal::SampleQualityNotZero {
            found: allocation.sample_quality,
        });
    }
    if allocation.plane_count != 1 {
        return Err(Refusal::AllocationPlaneCountNotOne {
            found: allocation.plane_count,
        });
    }
    let plane_zero = allocation.planes[0];
    let minimum_row_pitch = u64::from(allocation.width) * 4;
    let full_frame_span = u64::from(plane_zero.row_pitch)
        .checked_mul(u64::from(allocation.height))
        .ok_or(Refusal::PlaneFullFrameArithmeticOverflow {
            offset: plane_zero.offset,
            row_pitch: plane_zero.row_pitch,
            height: allocation.height,
            minimum_row_pitch,
        })?;
    if plane_zero.row_pitch & 3 != 0 {
        return Err(Refusal::PlaneRowPitchNotPixelAligned {
            found: plane_zero.row_pitch,
        });
    }
    if u64::from(plane_zero.row_pitch) < minimum_row_pitch {
        return Err(Refusal::PlaneRowPitchTooSmall {
            found: plane_zero.row_pitch,
            minimum: minimum_row_pitch,
        });
    }
    let full_frame_end = plane_zero.offset.checked_add(full_frame_span).ok_or(
        Refusal::PlaneFullFrameArithmeticOverflow {
            offset: plane_zero.offset,
            row_pitch: plane_zero.row_pitch,
            height: allocation.height,
            minimum_row_pitch,
        },
    )?;
    if plane_zero.offset > u64::from(u32::MAX) {
        return Err(Refusal::PlaneOffsetExceedsSetScanoutBlob {
            found: plane_zero.offset,
        });
    }
    if full_frame_end > allocation.byte_size {
        return Err(Refusal::PlaneFullFrameRangeExceedsBacking {
            end: full_frame_end,
            byte_size: allocation.byte_size,
        });
    }
    if u64::from(plane_zero.slice_pitch) < full_frame_span {
        return Err(Refusal::PlaneSlicePitchTooSmall {
            found: plane_zero.slice_pitch,
            minimum: full_frame_span,
        });
    }
    if !helios_hwa2_swizzle_is_scanout_bindable(allocation.swizzle_class) {
        return Err(Refusal::UnsupportedSwizzleClass {
            found: allocation.swizzle_class,
        });
    }
    // Only the LINEAR arm carries the Direct-Flip WIRE claim, so only it can be
    // held to the bit. An OPAQUE_OPTIMAL primary can never earn it -- §10.3
    // rules the class out and `admit_hwa2` therefore never stamps it -- and
    // requiring it here refused every UMD primary 32x/boot with
    // DisplayableFlagMissing sitting in front of it (22.22.341.0).
    if allocation.swizzle_class == HELIOS_HWA2_SWIZZLE_LINEAR
        && allocation.flags & HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE == 0
    {
        return Err(Refusal::DirectFlipCompatibleFlagMissing);
    }

    let runtime_primary = allocation.flags & HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY != 0;
    if !runtime_primary && allocation.vidpn_source != os_source {
        return Err(Refusal::AllocationSourceMismatch {
            allocation: allocation.vidpn_source,
            os: os_source,
        });
    }

    match plane {
        PlaneFacts::Classic => Ok(()),
        PlaneFacts::MpoCheck(plane) => validate_mpo_plane(mode, plane),
        PlaneFacts::MpoSet(set) => validate_mpo_set(mode, set),
    }
}

fn validate_mpo_set(mode: &CommittedMode, set: &MpoSetPlaneFacts) -> Result<(), Refusal> {
    validate_mpo_plane(mode, &set.plane)?;
    if !set.enabled {
        return Err(Refusal::MpoSetEnabledInputFlagMissing);
    }
    if set.context_count != 1 {
        return Err(Refusal::MpoContextCountNotOne {
            found: set.context_count,
        });
    }
    if !set.context_record_present {
        return Err(Refusal::MpoContextRecordMissing);
    }
    Ok(())
}

fn validate_mpo_plane(mode: &CommittedMode, plane: &MpoPlaneFacts) -> Result<(), Refusal> {
    if plane.plane_count != 1 {
        return Err(Refusal::MpoPlaneCountNotOne {
            found: plane.plane_count,
        });
    }
    if plane.layer_index != 0 {
        return Err(Refusal::MpoLayerNotZero {
            found: plane.layer_index,
        });
    }
    if plane.unsupported_or_reserved_attributes_or_features != 0 {
        return Err(Refusal::MpoUnsupportedOrReservedAttributesOrFeatures {
            found: plane.unsupported_or_reserved_attributes_or_features,
        });
    }
    let full_source = PlaneRect::full_output(mode.source_width, mode.source_height);
    let full_target = PlaneRect::full_output(mode.target_width, mode.target_height);
    if plane.source_rect != full_source {
        return Err(Refusal::MpoSourceRectNotFullOutput);
    }
    if plane.destination_rect != full_target {
        return Err(Refusal::MpoDestinationRectNotFullOutput);
    }
    if plane.clip_rect != full_target {
        return Err(Refusal::MpoClipRectNotFullOutput);
    }
    if !plane.identity_rotation {
        return Err(Refusal::MpoRotationNotIdentity);
    }
    if plane.vertical_flip {
        return Err(Refusal::MpoVerticalFlip);
    }
    if plane.horizontal_flip {
        return Err(Refusal::MpoHorizontalFlip);
    }
    if plane.alpha_blend {
        return Err(Refusal::MpoAlphaBlend);
    }
    if !plane.sdr_rgb {
        return Err(Refusal::MpoColorSpaceNotSdrRgb);
    }
    if plane.scaling {
        return Err(Refusal::MpoScaling);
    }
    if plane.post_composition {
        return Err(Refusal::MpoPostComposition);
    }
    if plane.hdr_metadata {
        return Err(Refusal::MpoHdrMetadata);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use helios_protocol::{
        HeliosWddmPlaneRecordV2, D3DDDIFMT_X8R8G8B8, HELIOS_HWA2_BIND_RENDER_TARGET,
        HELIOS_HWA2_BIND_SHADER_RESOURCE, HELIOS_HWA2_FLAG_SHARED,
        HELIOS_HWA2_KIND_STANDARD_SHADOW, HELIOS_HWA2_MEMORY_DEVICE_LOCAL,
        HELIOS_HWA2_SWIZZLE_LINEAR, HELIOS_HWA2_SWIZZLE_OPAQUE_OPTIMAL,
    };

    const WIDTH: u32 = 1920;
    const HEIGHT: u32 = 1080;
    const ROW_PITCH: u32 = WIDTH * 4;
    const FULL_FRAME_SPAN: u64 = (ROW_PITCH as u64) * (HEIGHT as u64);
    const MODE_GENERATION: u64 = 17;
    const BASE_FLAGS: u32 = HELIOS_HWA2_FLAG_PRIMARY
        | HELIOS_HWA2_FLAG_DISPLAYABLE
        | HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE
        | HELIOS_HWA2_FLAG_SHARED
        | HELIOS_HWA2_FLAG_STANDARD;

    #[derive(Clone, Copy)]
    struct Fixture {
        allocation: HeliosWddmAllocationDescV2,
        mode: CommittedMode,
        os_source: u32,
        operation: OperationFacts,
        plane: PlaneFacts,
    }

    fn allocation() -> HeliosWddmAllocationDescV2 {
        let mut allocation = HeliosWddmAllocationDescV2::header(HELIOS_PACKAGE_GENERATION, 0x51);
        allocation.byte_size = FULL_FRAME_SPAN + 4096;
        allocation.width = WIDTH;
        allocation.height = HEIGHT;
        allocation.depth_or_array_size = 1;
        allocation.mip_levels = 1;
        allocation.dxgi_format = DXGI_FORMAT_B8G8R8A8_UNORM;
        allocation.d3d_ddi_format = D3DDDIFMT_A8R8G8B8;
        allocation.sample_count = 1;
        allocation.sample_quality = 0;
        allocation.allocation_kind = HELIOS_HWA2_KIND_STANDARD_PRIMARY;
        allocation.flags = BASE_FLAGS;
        allocation.bind_flags = HELIOS_HWA2_BIND_RENDER_TARGET | HELIOS_HWA2_BIND_SHADER_RESOURCE;
        allocation.vidpn_source = 0;
        allocation.standard_allocation_type = 1;
        allocation.swizzle_class = HELIOS_HWA2_SWIZZLE_LINEAR;
        allocation.memory_class = HELIOS_HWA2_MEMORY_DEVICE_LOCAL;
        allocation.plane_count = 1;
        allocation.planes[0] = HeliosWddmPlaneRecordV2 {
            offset: 0,
            row_pitch: ROW_PITCH,
            slice_pitch: FULL_FRAME_SPAN as u32,
        };
        allocation
    }

    fn mpo_plane() -> MpoPlaneFacts {
        let full = PlaneRect::full_output(WIDTH, HEIGHT);
        MpoPlaneFacts {
            plane_count: 1,
            layer_index: 0,
            source_rect: full,
            destination_rect: full,
            clip_rect: full,
            identity_rotation: true,
            vertical_flip: false,
            horizontal_flip: false,
            alpha_blend: false,
            sdr_rgb: true,
            scaling: false,
            post_composition: false,
            hdr_metadata: false,
            unsupported_or_reserved_attributes_or_features: 0,
        }
    }

    fn fixture() -> Fixture {
        Fixture {
            allocation: allocation(),
            mode: CommittedMode {
                generation: MODE_GENERATION,
                source_id: 0,
                target_id: 7,
                source_width: WIDTH,
                source_height: HEIGHT,
                target_width: WIDTH,
                target_height: HEIGHT,
                active: true,
                visible: true,
                powered: true,
            },
            os_source: 0,
            operation: OperationFacts {
                current_mode_generation: MODE_GENERATION,
                adapter_match: HeliosAdapterMatch::SameAdapter,
                immediate_flip: false,
                stereo: false,
                shared_primary_transition: false,
                independent_flip_exclusive: false,
                mode_change: false,
                unsupported_or_reserved_flags: 0,
            },
            plane: PlaneFacts::MpoSet(MpoSetPlaneFacts {
                plane: mpo_plane(),
                enabled: true,
                context_count: 1,
                context_record_present: true,
            }),
        }
    }

    fn validate(fixture: &Fixture) -> Result<(), Refusal> {
        validate_direct_scanout_binding(
            &fixture.allocation,
            &fixture.mode,
            fixture.os_source,
            &fixture.operation,
            &fixture.plane,
        )
    }

    fn mpo_mut(fixture: &mut Fixture) -> &mut MpoPlaneFacts {
        let PlaneFacts::MpoSet(set) = &mut fixture.plane else {
            panic!("fixture must use the MPO Set path")
        };
        &mut set.plane
    }

    fn mpo_set_mut(fixture: &mut Fixture) -> &mut MpoSetPlaneFacts {
        let PlaneFacts::MpoSet(set) = &mut fixture.plane else {
            panic!("fixture must use the MPO Set path")
        };
        set
    }

    #[test]
    fn healthy_classic_check_and_set_paths_admit_without_an_ownership_token() {
        let mut fixture = fixture();
        assert_eq!(
            fixture
                .allocation
                .validate_create_output(HELIOS_PACKAGE_GENERATION),
            Ok(())
        );
        assert_eq!(validate(&fixture), Ok(()));

        fixture.plane = PlaneFacts::MpoCheck(mpo_plane());
        assert_eq!(validate(&fixture), Ok(()));

        fixture.plane = PlaneFacts::Classic;
        assert_eq!(validate(&fixture), Ok(()));
    }

    #[test]
    fn bgrx8_standard_primary_and_invisible_mode_change_flip_admit() {
        // The OS shared-primary shape as the KMD authors it: dxgi 88 (XR24),
        // programmed by the MODE_CHANGE flip before visibility is set.
        let mut fixture = fixture();
        fixture.allocation.dxgi_format = DXGI_FORMAT_B8G8R8X8_UNORM;
        fixture.mode.visible = false;
        fixture.operation.mode_change = true;
        fixture.plane = PlaneFacts::Classic;
        assert_eq!(
            fixture
                .allocation
                .validate_create_output(HELIOS_PACKAGE_GENERATION),
            Ok(())
        );
        assert_eq!(validate(&fixture), Ok(()));

        // The same invisible flip without MODE_CHANGE still refuses.
        fixture.operation.mode_change = false;
        assert_eq!(validate(&fixture), Err(Refusal::SourceInvisible));
    }

    #[test]
    fn exact_runtime_primary_sentinel_branch_admits() {
        let mut fixture = fixture();
        fixture.allocation.flags |= HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY;
        fixture.allocation.vidpn_source = D3DDDI_ID_UNINITIALIZED;
        assert_eq!(
            fixture
                .allocation
                .validate_create_output(HELIOS_PACKAGE_GENERATION),
            Ok(())
        );
        assert_eq!(validate(&fixture), Ok(()));
    }

    #[test]
    fn malformed_runtime_and_concrete_source_shapes_fail_as_final_hwa2() {
        let mut runtime = fixture();
        runtime.allocation.flags |= HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY;
        assert_eq!(
            validate(&runtime),
            Err(Refusal::InvalidFinalHwa2(
                HeliosAllocDescRejection::D3D12RuntimePrimaryNotSentinel { found: 0 }
            ))
        );

        let mut concrete = fixture();
        concrete.allocation.vidpn_source = D3DDDI_ID_UNINITIALIZED;
        assert_eq!(
            validate(&concrete),
            Err(Refusal::InvalidFinalHwa2(
                HeliosAllocDescRejection::PrimaryVidPnSourceNotConcrete
            ))
        );
    }

    #[test]
    fn ordinary_image_and_exact_standard_primary_semantics_both_admit() {
        let mut standard = fixture();
        assert_eq!(validate(&standard), Ok(()));

        standard.allocation.allocation_kind = HELIOS_HWA2_KIND_IMAGE;
        standard.allocation.flags &= !HELIOS_HWA2_FLAG_STANDARD;
        standard.allocation.standard_allocation_type = 0;
        assert_eq!(
            standard
                .allocation
                .validate_create_output(HELIOS_PACKAGE_GENERATION),
            Ok(())
        );
        assert_eq!(validate(&standard), Ok(()));
    }

    #[test]
    fn valid_transition_and_exclusive_facts_are_preserved_without_policy() {
        for (transition, exclusive) in [(false, false), (false, true), (true, false), (true, true)]
        {
            let mut fixture = fixture();
            fixture.operation.shared_primary_transition = transition;
            fixture.operation.independent_flip_exclusive = exclusive;
            assert_eq!(validate(&fixture), Ok(()));
        }
    }

    #[test]
    fn current_private_opaque_route_is_not_retirement_admission() {
        let mut fixture = fixture();
        fixture.allocation.swizzle_class = HELIOS_HWA2_SWIZZLE_OPAQUE_OPTIMAL;
        assert_eq!(
            validate(&fixture),
            Err(Refusal::InvalidFinalHwa2(
                HeliosAllocDescRejection::DirectFlipUnsupportedSwizzleClass {
                    found: HELIOS_HWA2_SWIZZLE_OPAQUE_OPTIMAL,
                }
            ))
        );
    }

    #[test]
    fn a_bounded_nonzero_plane_offset_remains_eligible() {
        let mut fixture = fixture();
        fixture.allocation.planes[0].offset = 4;
        assert_eq!(validate(&fixture), Ok(()));
    }

    #[test]
    fn padded_last_row_requires_the_complete_stride() {
        let mut fixture = fixture();
        fixture.allocation.width = 16;
        fixture.allocation.height = 16;
        fixture.allocation.byte_size = 1088;
        fixture.allocation.planes[0].row_pitch = 68;
        fixture.allocation.planes[0].slice_pitch = 1084;
        fixture.mode.source_width = 16;
        fixture.mode.source_height = 16;
        fixture.mode.target_width = 16;
        fixture.mode.target_height = 16;
        assert_eq!(
            validate(&fixture),
            Err(Refusal::PlaneSlicePitchTooSmall {
                found: 1084,
                minimum: 1088,
            })
        );
    }

    fn refusal_kind(refusal: Refusal) -> usize {
        match refusal {
            Refusal::InvalidFinalHwa2(_) => 0,
            Refusal::CommittedModeGenerationZero => 1,
            Refusal::CurrentModeGenerationZero => 2,
            Refusal::StaleCommittedModeGeneration { .. } => 3,
            Refusal::CommittedModeInactive => 4,
            Refusal::SourceInvisible => 5,
            Refusal::SourcePoweredOff => 6,
            Refusal::OsSourceUninitialized => 7,
            Refusal::CommittedSourceUninitialized => 8,
            Refusal::CommittedTargetUninitialized => 9,
            Refusal::CommittedSourceMismatch { .. } => 10,
            Refusal::AdapterDifferentOrUnknown => 11,
            Refusal::ImmediateFlipRequested => 12,
            Refusal::StereoOperationRequested => 13,
            Refusal::UnsupportedOrReservedOperationFlags { .. } => 14,
            Refusal::AllocationKindNotImageOrStandardPrimary { .. } => 15,
            Refusal::OrdinaryImageHasStandardSemantics { .. } => 16,
            Refusal::StandardPrimarySemanticsMismatch { .. } => 17,
            Refusal::PrimaryFlagMissing => 18,
            Refusal::DisplayableFlagMissing => 19,
            Refusal::DirectFlipCompatibleFlagMissing => 20,
            Refusal::StereoAllocation => 21,
            Refusal::ProtectedAllocation => 22,
            Refusal::CrossAdapterAllocation => 23,
            Refusal::FormatNotBgra8 { .. } => 24,
            Refusal::D3dDdiFormatNotA8R8G8B8 { .. } => 25,
            Refusal::CommittedSourceExtentZero => 26,
            Refusal::CommittedTargetExtentZero => 27,
            Refusal::CommittedSourceTargetExtentMismatch { .. } => 28,
            Refusal::AllocationSourceExtentMismatch { .. } => 29,
            Refusal::DepthOrArraySizeNotOne { .. } => 30,
            Refusal::MipLevelsNotOne { .. } => 31,
            Refusal::SampleCountNotOne { .. } => 32,
            Refusal::SampleQualityNotZero { .. } => 33,
            Refusal::AllocationPlaneCountNotOne { .. } => 34,
            Refusal::PlaneRowPitchTooSmall { .. } => 35,
            Refusal::PlaneRowPitchNotPixelAligned { .. } => 36,
            Refusal::PlaneFullFrameArithmeticOverflow { .. } => 37,
            Refusal::PlaneFullFrameRangeExceedsBacking { .. } => 38,
            Refusal::PlaneSlicePitchTooSmall { .. } => 39,
            Refusal::PlaneOffsetExceedsSetScanoutBlob { .. } => 40,
            Refusal::UnsupportedSwizzleClass { .. } => 41,
            Refusal::AllocationSourceMismatch { .. } => 42,
            Refusal::MpoPlaneCountNotOne { .. } => 43,
            Refusal::MpoLayerNotZero { .. } => 44,
            Refusal::MpoUnsupportedOrReservedAttributesOrFeatures { .. } => 45,
            Refusal::MpoSetEnabledInputFlagMissing => 46,
            Refusal::MpoContextCountNotOne { .. } => 47,
            Refusal::MpoContextRecordMissing => 48,
            Refusal::MpoSourceRectNotFullOutput => 49,
            Refusal::MpoDestinationRectNotFullOutput => 50,
            Refusal::MpoClipRectNotFullOutput => 51,
            Refusal::MpoRotationNotIdentity => 52,
            Refusal::MpoVerticalFlip => 53,
            Refusal::MpoHorizontalFlip => 54,
            Refusal::MpoAlphaBlend => 55,
            Refusal::MpoColorSpaceNotSdrRgb => 56,
            Refusal::MpoScaling => 57,
            Refusal::MpoPostComposition => 58,
            Refusal::MpoHdrMetadata => 59,
        }
    }

    struct MutationCase {
        name: &'static str,
        mutate: fn(&mut Fixture),
        expected: Refusal,
    }

    macro_rules! mutation_cases {
        ($( $name:ident($fixture:ident) $body:block => $expected:expr; )+) => {
            $(fn $name($fixture: &mut Fixture) $body)+

            const MUTATION_CASES: &[MutationCase] = &[
                $(MutationCase {
                    name: stringify!($name),
                    mutate: $name,
                    expected: $expected,
                },)+
            ];
        };
    }

    mutation_cases! {
        invalid_final_hwa2(f) {
            f.allocation.byte_size = u64::from(f.allocation.planes[0].slice_pitch) - 1;
        } => Refusal::InvalidFinalHwa2(HeliosAllocDescRejection::PlaneRangeExceedsByteSize { index: 0 });
        committed_generation_zero(f) { f.mode.generation = 0; }
            => Refusal::CommittedModeGenerationZero;
        current_generation_zero(f) { f.operation.current_mode_generation = 0; }
            => Refusal::CurrentModeGenerationZero;
        stale_generation(f) { f.operation.current_mode_generation = MODE_GENERATION + 1; }
            => Refusal::StaleCommittedModeGeneration { committed: MODE_GENERATION, current: MODE_GENERATION + 1 };
        inactive(f) { f.mode.active = false; }
            => Refusal::CommittedModeInactive;
        invisible(f) { f.mode.visible = false; }
            => Refusal::SourceInvisible;
        powered_off(f) { f.mode.powered = false; }
            => Refusal::SourcePoweredOff;
        os_source_uninitialized(f) { f.os_source = D3DDDI_ID_UNINITIALIZED; }
            => Refusal::OsSourceUninitialized;
        committed_source_uninitialized(f) { f.mode.source_id = D3DDDI_ID_UNINITIALIZED; }
            => Refusal::CommittedSourceUninitialized;
        committed_target_uninitialized(f) { f.mode.target_id = D3DDDI_ID_UNINITIALIZED; }
            => Refusal::CommittedTargetUninitialized;
        committed_source_mismatch(f) { f.mode.source_id = 1; }
            => Refusal::CommittedSourceMismatch { committed: 1, os: 0 };
        different_adapter(f) { f.operation.adapter_match = HeliosAdapterMatch::DifferentOrUnknown; }
            => Refusal::AdapterDifferentOrUnknown;
        immediate(f) { f.operation.immediate_flip = true; }
            => Refusal::ImmediateFlipRequested;
        stereo_operation(f) { f.operation.stereo = true; }
            => Refusal::StereoOperationRequested;
        unsupported_operation_flags(f) { f.operation.unsupported_or_reserved_flags = 0x8000_0000; }
            => Refusal::UnsupportedOrReservedOperationFlags { found: 0x8000_0000 };
        unsupported_kind(f) { f.allocation.allocation_kind = HELIOS_HWA2_KIND_STANDARD_SHADOW; }
            => Refusal::AllocationKindNotImageOrStandardPrimary {
                found: HELIOS_HWA2_KIND_STANDARD_SHADOW,
            };
        ordinary_image_with_standard_semantics(f) { f.allocation.allocation_kind = HELIOS_HWA2_KIND_IMAGE; }
            => Refusal::OrdinaryImageHasStandardSemantics {
                flags: BASE_FLAGS,
                standard_allocation_type: SHARED_PRIMARY_STANDARD_ALLOCATION_TYPE,
            };
        standard_primary_wrong_type(f) { f.allocation.standard_allocation_type = 4; }
            => Refusal::StandardPrimarySemanticsMismatch {
                flags: BASE_FLAGS,
                standard_allocation_type: 4,
            };
        missing_primary(f) {
            f.allocation.flags &= !(HELIOS_HWA2_FLAG_PRIMARY
                | HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE);
            f.allocation.vidpn_source = D3DDDI_ID_UNINITIALIZED;
        }
            => Refusal::PrimaryFlagMissing;
        missing_displayable(f) { f.allocation.flags &= !HELIOS_HWA2_FLAG_DISPLAYABLE; }
            => Refusal::DisplayableFlagMissing;
        stereo_allocation(f) { f.allocation.flags |= HELIOS_HWA2_FLAG_STEREO; }
            => Refusal::StereoAllocation;
        protected_allocation(f) {
            f.allocation.flags = (f.allocation.flags | HELIOS_HWA2_FLAG_PROTECTED)
                & !HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE;
        }
            => Refusal::ProtectedAllocation;
        cross_adapter_allocation(f) {
            f.allocation.flags = (f.allocation.flags | HELIOS_HWA2_FLAG_CROSS_ADAPTER)
                & !HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE;
        }
            => Refusal::CrossAdapterAllocation;
        wrong_format(f) { f.allocation.dxgi_format = 28; }
            => Refusal::FormatNotBgra8 { found: 28 };
        wrong_d3d_ddi_format(f) { f.allocation.d3d_ddi_format = D3DDDIFMT_X8R8G8B8; }
            => Refusal::D3dDdiFormatNotA8R8G8B8 { found: D3DDDIFMT_X8R8G8B8 };
        committed_source_extent_zero(f) { f.mode.source_width = 0; }
            => Refusal::CommittedSourceExtentZero;
        committed_target_extent_zero(f) { f.mode.target_width = 0; }
            => Refusal::CommittedTargetExtentZero;
        committed_source_target_extent_mismatch(f) { f.mode.target_width = WIDTH - 1; }
            => Refusal::CommittedSourceTargetExtentMismatch {
                source_width: WIDTH,
                source_height: HEIGHT,
                target_width: WIDTH - 1,
                target_height: HEIGHT,
            };
        allocation_source_extent_mismatch(f) { f.allocation.width = WIDTH - 1; }
            => Refusal::AllocationSourceExtentMismatch {
                allocation_width: WIDTH - 1,
                allocation_height: HEIGHT,
                source_width: WIDTH,
                source_height: HEIGHT,
            };
        array_size(f) { f.allocation.depth_or_array_size = 2; }
            => Refusal::DepthOrArraySizeNotOne { found: 2 };
        mip_levels(f) { f.allocation.mip_levels = 2; }
            => Refusal::MipLevelsNotOne { found: 2 };
        sample_count(f) { f.allocation.sample_count = 2; }
            => Refusal::SampleCountNotOne { found: 2 };
        sample_quality(f) { f.allocation.sample_quality = 1; }
            => Refusal::SampleQualityNotZero { found: 1 };
        allocation_plane_count(f) {
            f.allocation.flags &= !HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE;
            f.allocation.plane_count = 2;
            f.allocation.planes[1] = f.allocation.planes[0];
        }
            => Refusal::AllocationPlaneCountNotOne { found: 2 };
        plane_row_pitch(f) { f.allocation.planes[0].row_pitch = ROW_PITCH - 4; }
            => Refusal::PlaneRowPitchTooSmall {
                found: ROW_PITCH - 4,
                minimum: (WIDTH as u64) * 4,
            };
        plane_row_pitch_not_pixel_aligned(f) { f.allocation.planes[0].row_pitch = ROW_PITCH + 1; }
            => Refusal::PlaneRowPitchNotPixelAligned { found: ROW_PITCH + 1 };
        plane_full_frame_arithmetic_overflow(f) {
            f.allocation.byte_size = u64::MAX;
            f.allocation.planes[0].offset = u64::MAX - FULL_FRAME_SPAN + 1;
            f.allocation.planes[0].slice_pitch = ROW_PITCH;
        } => Refusal::PlaneFullFrameArithmeticOverflow {
            offset: u64::MAX - FULL_FRAME_SPAN + 1,
            row_pitch: ROW_PITCH,
            height: HEIGHT,
            minimum_row_pitch: (WIDTH as u64) * 4,
        };
        plane_full_frame_exceeds_backing(f) {
            f.allocation.byte_size = FULL_FRAME_SPAN - 1;
            f.allocation.planes[0].slice_pitch = ROW_PITCH;
        } => Refusal::PlaneFullFrameRangeExceedsBacking {
            end: FULL_FRAME_SPAN,
            byte_size: FULL_FRAME_SPAN - 1,
        };
        plane_slice_pitch(f) { f.allocation.planes[0].slice_pitch = FULL_FRAME_SPAN as u32 - 1; }
            => Refusal::PlaneSlicePitchTooSmall {
                found: FULL_FRAME_SPAN as u32 - 1,
                minimum: FULL_FRAME_SPAN,
            };
        plane_offset_exceeds_set_scanout_blob(f) {
            f.allocation.planes[0].offset = u64::from(u32::MAX) + 1;
            f.allocation.byte_size = u64::from(u32::MAX) + 1 + FULL_FRAME_SPAN;
        } => Refusal::PlaneOffsetExceedsSetScanoutBlob {
            found: (u32::MAX as u64) + 1,
        };
        missing_direct_flip(f) { f.allocation.flags &= !HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE; }
            => Refusal::DirectFlipCompatibleFlagMissing;
        allocation_source_mismatch(f) { f.allocation.vidpn_source = 1; }
            => Refusal::AllocationSourceMismatch { allocation: 1, os: 0 };
        mpo_plane_count(f) { mpo_mut(f).plane_count = 2; }
            => Refusal::MpoPlaneCountNotOne { found: 2 };
        mpo_layer(f) { mpo_mut(f).layer_index = 1; }
            => Refusal::MpoLayerNotZero { found: 1 };
        mpo_unsupported_attributes_or_features(f) {
            mpo_mut(f).unsupported_or_reserved_attributes_or_features = 1 << 63;
        } => Refusal::MpoUnsupportedOrReservedAttributesOrFeatures { found: 1 << 63 };
        mpo_enabled_input_flag_missing(f) { mpo_set_mut(f).enabled = false; }
            => Refusal::MpoSetEnabledInputFlagMissing;
        mpo_context_count(f) { mpo_set_mut(f).context_count = 2; }
            => Refusal::MpoContextCountNotOne { found: 2 };
        mpo_context_missing(f) { mpo_set_mut(f).context_record_present = false; }
            => Refusal::MpoContextRecordMissing;
        mpo_source_rect(f) { mpo_mut(f).source_rect.left = 1; }
            => Refusal::MpoSourceRectNotFullOutput;
        mpo_destination_rect(f) { mpo_mut(f).destination_rect.right -= 1; }
            => Refusal::MpoDestinationRectNotFullOutput;
        mpo_clip_rect(f) { mpo_mut(f).clip_rect.bottom -= 1; }
            => Refusal::MpoClipRectNotFullOutput;
        mpo_rotation(f) { mpo_mut(f).identity_rotation = false; }
            => Refusal::MpoRotationNotIdentity;
        mpo_vertical_flip(f) { mpo_mut(f).vertical_flip = true; }
            => Refusal::MpoVerticalFlip;
        mpo_horizontal_flip(f) { mpo_mut(f).horizontal_flip = true; }
            => Refusal::MpoHorizontalFlip;
        mpo_alpha(f) { mpo_mut(f).alpha_blend = true; }
            => Refusal::MpoAlphaBlend;
        mpo_color(f) { mpo_mut(f).sdr_rgb = false; }
            => Refusal::MpoColorSpaceNotSdrRgb;
        mpo_scaling(f) { mpo_mut(f).scaling = true; }
            => Refusal::MpoScaling;
        mpo_post_composition(f) { mpo_mut(f).post_composition = true; }
            => Refusal::MpoPostComposition;
        mpo_hdr(f) { mpo_mut(f).hdr_metadata = true; }
            => Refusal::MpoHdrMetadata;
    }

    #[test]
    fn single_field_mutation_matrix_exercises_every_named_refusal() {
        const REFUSAL_COUNT: usize = 60;
        let mut seen = [false; REFUSAL_COUNT];

        for case in MUTATION_CASES {
            let mut fixture = fixture();
            (case.mutate)(&mut fixture);
            let actual = validate(&fixture);
            assert_eq!(actual, Err(case.expected), "{}", case.name);
            let kind = refusal_kind(case.expected);
            assert!(!seen[kind], "duplicate refusal kind for {}", case.name);
            seen[kind] = true;
        }

        // `UnsupportedSwizzleClass` is the one refusal with no mutation case,
        // and deliberately: every class `validate_create_output` admits is now
        // scanout-bindable, so nothing can reach it through this entry point.
        // `unsupported_swizzle_class_is_unreachable_by_construction` states that
        // as a property instead of pretending a case exercises it.
        const UNREACHABLE: usize = 41;
        assert_eq!(MUTATION_CASES.len(), REFUSAL_COUNT - 1);
        assert!(!seen[UNREACHABLE]);
        assert!(seen
            .into_iter()
            .enumerate()
            .all(|(kind, present)| present || kind == UNREACHABLE));
    }

    /// The relaxation that admits the OPTIMAL direct-scanout primary, stated as
    /// the property that makes the refusal above unreachable.
    #[test]
    fn unsupported_swizzle_class_is_unreachable_by_construction() {
        for class in [HELIOS_HWA2_SWIZZLE_LINEAR, HELIOS_HWA2_SWIZZLE_OPAQUE_OPTIMAL] {
            assert!(helios_hwa2_swizzle_is_scanout_bindable(class), "{class}");
        }
        assert_eq!(
            refusal_kind(Refusal::UnsupportedSwizzleClass { found: 0 }),
            41
        );
    }

    /// The UMD's direct-scanout primary: OPAQUE_OPTIMAL, and therefore never
    /// `DIRECT_FLIP_COMPATIBLE` (§10.3 rules the class out, so `admit_hwa2`
    /// cannot stamp it). Requiring the bit here refused DWM's primary 32x/boot.
    #[test]
    fn optimal_primary_without_the_direct_flip_bit_is_admitted() {
        let mut fixture = fixture();
        fixture.allocation.flags &= !HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE;
        fixture.allocation.swizzle_class = HELIOS_HWA2_SWIZZLE_OPAQUE_OPTIMAL;
        assert_eq!(validate(&fixture), Ok(()));
    }

    /// The LINEAR arm is unchanged: it carries the Direct-Flip wire claim, so it
    /// is still held to the bit.
    #[test]
    fn linear_primary_still_requires_the_direct_flip_bit() {
        let mut fixture = fixture();
        fixture.allocation.flags &= !HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE;
        assert_eq!(fixture.allocation.swizzle_class, HELIOS_HWA2_SWIZZLE_LINEAR);
        assert_eq!(
            validate(&fixture),
            Err(Refusal::DirectFlipCompatibleFlagMissing)
        );
    }
}

#[cfg(test)]
mod refusal_code_tests {
    use super::Refusal as R;
    use super::*;

    /// Every `Refusal`, so the uniqueness check below cannot silently skip one.
    /// A new variant fails to compile in `refusal_code_and_detail` (the match is
    /// exhaustive) and fails the count assert here.
    const ALL: &[Refusal] = &[
        R::InvalidFinalHwa2(HeliosAllocDescRejection::ByteSizeZero),
        R::CommittedModeGenerationZero,
        R::CurrentModeGenerationZero,
        R::StaleCommittedModeGeneration { committed: 0, current: 0 },
        R::CommittedModeInactive,
        R::SourceInvisible,
        R::SourcePoweredOff,
        R::OsSourceUninitialized,
        R::CommittedSourceUninitialized,
        R::CommittedTargetUninitialized,
        R::CommittedSourceMismatch { committed: 0, os: 0 },
        R::AdapterDifferentOrUnknown,
        R::ImmediateFlipRequested,
        R::StereoOperationRequested,
        R::UnsupportedOrReservedOperationFlags { found: 0 },
        R::AllocationKindNotImageOrStandardPrimary { found: 0 },
        R::OrdinaryImageHasStandardSemantics { flags: 0, standard_allocation_type: 0 },
        R::StandardPrimarySemanticsMismatch { flags: 0, standard_allocation_type: 0 },
        R::PrimaryFlagMissing,
        R::DisplayableFlagMissing,
        R::DirectFlipCompatibleFlagMissing,
        R::StereoAllocation,
        R::ProtectedAllocation,
        R::CrossAdapterAllocation,
        R::FormatNotBgra8 { found: 0 },
        R::D3dDdiFormatNotA8R8G8B8 { found: 0 },
        R::CommittedSourceExtentZero,
        R::CommittedTargetExtentZero,
        R::CommittedSourceTargetExtentMismatch { source_width: 0, source_height: 0, target_width: 0, target_height: 0 },
        R::AllocationSourceExtentMismatch { allocation_width: 0, allocation_height: 0, source_width: 0, source_height: 0 },
        R::DepthOrArraySizeNotOne { found: 0 },
        R::MipLevelsNotOne { found: 0 },
        R::SampleCountNotOne { found: 0 },
        R::SampleQualityNotZero { found: 0 },
        R::AllocationPlaneCountNotOne { found: 0 },
        R::PlaneRowPitchTooSmall { found: 0, minimum: 0 },
        R::PlaneRowPitchNotPixelAligned { found: 0 },
        R::PlaneFullFrameArithmeticOverflow { offset: 0, row_pitch: 0, height: 0, minimum_row_pitch: 0 },
        R::PlaneFullFrameRangeExceedsBacking { end: 0, byte_size: 0 },
        R::PlaneSlicePitchTooSmall { found: 0, minimum: 0 },
        R::PlaneOffsetExceedsSetScanoutBlob { found: 0 },
        R::UnsupportedSwizzleClass { found: 0 },
        R::AllocationSourceMismatch { allocation: 0, os: 0 },
        R::MpoPlaneCountNotOne { found: 0 },
        R::MpoLayerNotZero { found: 0 },
        R::MpoUnsupportedOrReservedAttributesOrFeatures { found: 0 },
        R::MpoSetEnabledInputFlagMissing,
        R::MpoContextCountNotOne { found: 0 },
        R::MpoContextRecordMissing,
        R::MpoSourceRectNotFullOutput,
        R::MpoDestinationRectNotFullOutput,
        R::MpoClipRectNotFullOutput,
        R::MpoRotationNotIdentity,
        R::MpoVerticalFlip,
        R::MpoHorizontalFlip,
        R::MpoAlphaBlend,
        R::MpoColorSpaceNotSdrRgb,
        R::MpoScaling,
        R::MpoPostComposition,
        R::MpoHdrMetadata,
    ];

    #[test]
    fn every_refusal_has_a_distinct_code_in_range() {
        assert_eq!(ALL.len(), 60, "add the new Refusal to ALL");
        let mut seen = [false; 256];
        for refusal in ALL {
            let (code, _) = refusal_code_and_detail(*refusal);
            assert!(
                (0x40..=0x7b).contains(&code),
                "code {code:#x} outside 0x40..=0x7B for {refusal:?}"
            );
            assert!(!seen[code as usize], "duplicate code {code:#x} at {refusal:?}");
            seen[code as usize] = true;
        }
    }

    /// 0x7F was the catch-all these codes replace; nothing may collide with it.
    #[test]
    fn no_code_collides_with_the_retired_catch_all() {
        for refusal in ALL {
            assert_ne!(refusal_code_and_detail(*refusal).0, 0x7f);
        }
    }

    /// The fourteen codes already published in `D2AdmWhy` keep their values, so
    /// counter values recorded before this change still decode.
    #[test]
    fn published_codes_are_unchanged() {
        use Refusal as R;
        for (refusal, code) in [
            (R::SourceInvisible, 0x45),
            (R::SourcePoweredOff, 0x46),
            (R::CommittedSourceMismatch { committed: 0, os: 0 }, 0x4a),
            (R::UnsupportedOrReservedOperationFlags { found: 0 }, 0x4e),
            (
                R::StandardPrimarySemanticsMismatch { flags: 0, standard_allocation_type: 0 },
                0x51,
            ),
            (R::DirectFlipCompatibleFlagMissing, 0x54),
            (R::FormatNotBgra8 { found: 0 }, 0x58),
            (R::D3dDdiFormatNotA8R8G8B8 { found: 0 }, 0x59),
            (
                R::AllocationSourceExtentMismatch {
                    allocation_width: 0,
                    allocation_height: 0,
                    source_width: 0,
                    source_height: 0,
                },
                0x5d,
            ),
            (R::PlaneRowPitchTooSmall { found: 0, minimum: 0 }, 0x63),
            (R::PlaneFullFrameRangeExceedsBacking { end: 0, byte_size: 0 }, 0x66),
            (R::PlaneSlicePitchTooSmall { found: 0, minimum: 0 }, 0x67),
            (R::UnsupportedSwizzleClass { found: 0 }, 0x69),
            (R::AllocationSourceMismatch { allocation: 0, os: 0 }, 0x6a),
        ] {
            assert_eq!(refusal_code_and_detail(refusal).0, code, "{refusal:?}");
        }
    }

    #[test]
    fn detail_carries_the_diagnostic_scalar() {
        let (code, detail) =
            refusal_code_and_detail(Refusal::AllocationKindNotImageOrStandardPrimary { found: 9 });
        assert_eq!((code, detail), (0x4f, 9));
        let (_, detail) = refusal_code_and_detail(Refusal::AllocationSourceExtentMismatch {
            allocation_width: 1280,
            allocation_height: 896,
            source_width: 1280,
            source_height: 800,
        });
        assert_eq!(detail, (1280 << 16) | 896);
    }
}
