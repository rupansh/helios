#!/usr/bin/env python3
"""Decode the KMD scanout-admission refusal counters.

Usage: decode-admission-refusals.py D2AdmSL D2AdmSH [D2AdmWhy] [D2AdmDat]

Values are the decimal registry values under
HKLM\\SYSTEM\\CurrentControlSet\\Services\\helios_kmd_render. D2AdmSL/SH are a
bitset over (code - 0x40); D2AdmWhy is the LAST code only. Codes come from
kmd_logic refusal_code_and_detail() -- variant declaration index + 63.
"""
import sys

CODES = {
    0x40: "InvalidFinalHwa2",
    0x41: "CommittedModeGenerationZero",
    0x42: "CurrentModeGenerationZero",
    0x43: "StaleCommittedModeGeneration",
    0x44: "CommittedModeInactive",
    0x45: "SourceInvisible",
    0x46: "SourcePoweredOff",
    0x47: "OsSourceUninitialized",
    0x48: "CommittedSourceUninitialized",
    0x49: "CommittedTargetUninitialized",
    0x4A: "CommittedSourceMismatch",
    0x4B: "AdapterDifferentOrUnknown",
    0x4C: "ImmediateFlipRequested",
    0x4D: "StereoOperationRequested",
    0x4E: "UnsupportedOrReservedOperationFlags",
    0x4F: "AllocationKindNotImageOrStandardPrimary",
    0x50: "OrdinaryImageHasStandardSemantics",
    0x51: "StandardPrimarySemanticsMismatch",
    0x52: "PrimaryFlagMissing",
    0x53: "DisplayableFlagMissing",
    0x54: "DirectFlipCompatibleFlagMissing",
    0x55: "StereoAllocation",
    0x56: "ProtectedAllocation",
    0x57: "CrossAdapterAllocation",
    0x58: "FormatNotBgra8",
    0x59: "D3dDdiFormatNotA8R8G8B8",
    0x5A: "CommittedSourceExtentZero",
    0x5B: "CommittedTargetExtentZero",
    0x5C: "CommittedSourceTargetExtentMismatch",
    0x5D: "AllocationSourceExtentMismatch",
    0x5E: "DepthOrArraySizeNotOne",
    0x5F: "MipLevelsNotOne",
    0x60: "SampleCountNotOne",
    0x61: "SampleQualityNotZero",
    0x62: "AllocationPlaneCountNotOne",
    0x63: "PlaneRowPitchTooSmall",
    0x64: "PlaneRowPitchNotPixelAligned",
    0x65: "PlaneFullFrameArithmeticOverflow",
    0x66: "PlaneFullFrameRangeExceedsBacking",
    0x67: "PlaneSlicePitchTooSmall",
    0x68: "PlaneOffsetExceedsSetScanoutBlob",
    0x69: "UnsupportedSwizzleClass",
    0x6A: "AllocationSourceMismatch",
    0x6B: "MpoPlaneCountNotOne",
    0x6C: "MpoLayerNotZero",
    0x6D: "MpoUnsupportedOrReservedAttributesOrFeatures",
    0x6E: "MpoSetEnabledInputFlagMissing",
    0x6F: "MpoContextCountNotOne",
    0x70: "MpoContextRecordMissing",
    0x71: "MpoSourceRectNotFullOutput",
    0x72: "MpoDestinationRectNotFullOutput",
    0x73: "MpoClipRectNotFullOutput",
    0x74: "MpoRotationNotIdentity",
    0x75: "MpoVerticalFlip",
    0x76: "MpoHorizontalFlip",
    0x77: "MpoAlphaBlend",
    0x78: "MpoColorSpaceNotSdrRgb",
    0x79: "MpoScaling",
    0x7A: "MpoPostComposition",
    0x7B: "MpoHdrMetadata",
}


def main() -> int:
    a = sys.argv[1:]
    if not 2 <= len(a) <= 4:
        print(__doc__)
        return 2
    lo, hi = int(a[0], 0), int(a[1], 0)
    mask = lo | (hi << 32)
    seen = [0x40 + b for b in range(60) if mask >> b & 1]
    if not seen:
        print("no admission refusal recorded this boot")
    for code in seen:
        print(f"  {code:#04x}  {CODES.get(code, '?UNKNOWN?')}")
    if len(a) >= 3:
        last = int(a[2], 0)
        print(f"last: {last:#04x}  {CODES.get(last, '?UNKNOWN?')}"
              + (f"   detail={int(a[3], 0)} ({int(a[3], 0):#x})" if len(a) == 4 else ""))
    if len(seen) > 1:
        print(f"MIXED: {len(seen)} distinct reasons -- D2AdmWhy names only the last")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
