#ifndef HELIOS_NATIVE_FENCE_H
#define HELIOS_NATIVE_FENCE_H

#include <stddef.h>
#include <stdint.h>

#if defined(__cplusplus)
#define HELIOS_NF_STATIC_ASSERT static_assert
#define HELIOS_NF_ALIGNOF(type) alignof(type)
#else
#define HELIOS_NF_STATIC_ASSERT _Static_assert
#define HELIOS_NF_ALIGNOF(type) _Alignof(type)
#endif

#ifndef HELIOS_PACKAGE_GENERATION
#define HELIOS_PACKAGE_GENERATION_TAG     0x48454C49u
#define HELIOS_PACKAGE_GENERATION_ORDINAL 4u
#define HELIOS_PACKAGE_GENERATION \
    ((((uint64_t)HELIOS_PACKAGE_GENERATION_TAG) << 32) | \
     (uint64_t)HELIOS_PACKAGE_GENERATION_ORDINAL)
#endif

HELIOS_NF_STATIC_ASSERT(HELIOS_PACKAGE_GENERATION == UINT64_C(0x48454C4900000004),
                        "package generation must equal protocol/src/lib.rs "
                        "HELIOS_PACKAGE_GENERATION");

#define HELIOS_HNF1_MAGIC UINT32_C(0x31464e48)
#define HELIOS_HNF1_ABI_VERSION UINT16_C(1)
#define HELIOS_HNF1_SIZE 64u
#define HELIOS_HNF1_FLAG_SHARED (UINT32_C(1) << 0)
#define HELIOS_HNF1_FLAGS_RESERVED_MASK (~HELIOS_HNF1_FLAG_SHARED)
#define HELIOS_NATIVE_FENCE_TYPE_DEFAULT 0u
#define HELIOS_NATIVE_FENCE_TYPE_INTRA_GPU 1u

typedef struct HeliosNativeFencePddV1 {
    uint32_t magic;
    uint16_t abi_version;
    uint16_t struct_size;
    uint64_t package_generation;
    uint64_t object_generation;
    uint32_t native_type;
    uint32_t flags;
    int64_t adapter_luid;
    uint8_t reserved[24];
} HeliosNativeFencePddV1;

HELIOS_NF_STATIC_ASSERT(sizeof(HeliosNativeFencePddV1) == HELIOS_HNF1_SIZE,
                        "HeliosNativeFencePddV1 size");
HELIOS_NF_STATIC_ASSERT(HELIOS_NF_ALIGNOF(HeliosNativeFencePddV1) == 8,
                        "HeliosNativeFencePddV1 alignment");
HELIOS_NF_STATIC_ASSERT(offsetof(HeliosNativeFencePddV1, magic) == 0, "HNF1 magic");
HELIOS_NF_STATIC_ASSERT(offsetof(HeliosNativeFencePddV1, abi_version) == 4, "HNF1 abi_version");
HELIOS_NF_STATIC_ASSERT(offsetof(HeliosNativeFencePddV1, struct_size) == 6, "HNF1 struct_size");
HELIOS_NF_STATIC_ASSERT(offsetof(HeliosNativeFencePddV1, package_generation) == 8,
                        "HNF1 package_generation");
HELIOS_NF_STATIC_ASSERT(offsetof(HeliosNativeFencePddV1, object_generation) == 16,
                        "HNF1 object_generation");
HELIOS_NF_STATIC_ASSERT(offsetof(HeliosNativeFencePddV1, native_type) == 24,
                        "HNF1 native_type");
HELIOS_NF_STATIC_ASSERT(offsetof(HeliosNativeFencePddV1, flags) == 28, "HNF1 flags");
HELIOS_NF_STATIC_ASSERT(offsetof(HeliosNativeFencePddV1, adapter_luid) == 32,
                        "HNF1 adapter_luid");
HELIOS_NF_STATIC_ASSERT(offsetof(HeliosNativeFencePddV1, reserved) == 40, "HNF1 reserved");

#endif
