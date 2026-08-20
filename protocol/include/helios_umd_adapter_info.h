#ifndef HELIOS_UMD_ADAPTER_INFO_H
#define HELIOS_UMD_ADAPTER_INFO_H

/* Generated projection of protocol/src/umd_adapter_info.rs. */

#include <stddef.h>
#include <stdint.h>

#include "helios_wddm.h"

#define HELIOS_UMD_ADAPTER_INFO_MAGIC       0x31494148u
#define HELIOS_UMD_ADAPTER_INFO_ABI_VERSION 1u
#define HELIOS_UMD_ADAPTER_INFO_BYTES       48u

typedef struct HeliosUmdAdapterInfoV1 {
    uint32_t magic;
    uint32_t struct_bytes;
    uint32_t abi_version;
    uint32_t reserved0;
    uint64_t package_generation;
    uint64_t adapter_generation;
    int64_t adapter_luid;
    uint64_t reserved1;
} HeliosUmdAdapterInfoV1;

#if defined(__cplusplus)
#define HELIOS_ADAPTER_INFO_STATIC_ASSERT(cond, msg) static_assert(cond, msg)
#define HELIOS_ADAPTER_INFO_ALIGNOF(type)            alignof(type)
#else
#define HELIOS_ADAPTER_INFO_STATIC_ASSERT(cond, msg) _Static_assert(cond, msg)
#define HELIOS_ADAPTER_INFO_ALIGNOF(type)            _Alignof(type)
#endif

HELIOS_ADAPTER_INFO_STATIC_ASSERT(sizeof(HeliosUmdAdapterInfoV1) == 48, "");
HELIOS_ADAPTER_INFO_STATIC_ASSERT(HELIOS_ADAPTER_INFO_ALIGNOF(HeliosUmdAdapterInfoV1) == 8, "");
HELIOS_ADAPTER_INFO_STATIC_ASSERT(offsetof(HeliosUmdAdapterInfoV1, magic) == 0, "");
HELIOS_ADAPTER_INFO_STATIC_ASSERT(offsetof(HeliosUmdAdapterInfoV1, struct_bytes) == 4, "");
HELIOS_ADAPTER_INFO_STATIC_ASSERT(offsetof(HeliosUmdAdapterInfoV1, abi_version) == 8, "");
HELIOS_ADAPTER_INFO_STATIC_ASSERT(offsetof(HeliosUmdAdapterInfoV1, reserved0) == 12, "");
HELIOS_ADAPTER_INFO_STATIC_ASSERT(offsetof(HeliosUmdAdapterInfoV1, package_generation) == 16, "");
HELIOS_ADAPTER_INFO_STATIC_ASSERT(offsetof(HeliosUmdAdapterInfoV1, adapter_generation) == 24, "");
HELIOS_ADAPTER_INFO_STATIC_ASSERT(offsetof(HeliosUmdAdapterInfoV1, adapter_luid) == 32, "");
HELIOS_ADAPTER_INFO_STATIC_ASSERT(offsetof(HeliosUmdAdapterInfoV1, reserved1) == 40, "");
HELIOS_ADAPTER_INFO_STATIC_ASSERT((HELIOS_UMD_ADAPTER_INFO_MAGIC & 0xffu) == (unsigned char)'H', "");
HELIOS_ADAPTER_INFO_STATIC_ASSERT(((HELIOS_UMD_ADAPTER_INFO_MAGIC >> 8) & 0xffu) == (unsigned char)'A', "");
HELIOS_ADAPTER_INFO_STATIC_ASSERT(((HELIOS_UMD_ADAPTER_INFO_MAGIC >> 16) & 0xffu) == (unsigned char)'I', "");
HELIOS_ADAPTER_INFO_STATIC_ASSERT(((HELIOS_UMD_ADAPTER_INFO_MAGIC >> 24) & 0xffu) == (unsigned char)'1', "");

#endif /* HELIOS_UMD_ADAPTER_INFO_H */
