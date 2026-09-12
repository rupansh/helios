// Native Helios probe naming contract. Full-path and SHA256 verification is
// performed by the runner; recognizing a filename alone is not attribution.
#pragma once

#include <cstddef>

inline wchar_t helios_native_ascii_lower(wchar_t value)
{
    return value >= L'A' && value <= L'Z' ? value + (L'a' - L'A') : value;
}

inline bool helios_native_umd12_name(const wchar_t *name)
{
    if (!name) return false;
    const wchar_t base[] = L"helios_umd12";
    std::size_t offset = 0;
    for (; base[offset]; ++offset)
        if (helios_native_ascii_lower(name[offset]) != base[offset]) return false;

    // The package uses the plain name. ProgramData hotplug uses precisely
    // sixteen hexadecimal SHA256 characters, followed by the DLL extension.
    if (name[offset] == L'_') {
        ++offset;
        for (unsigned i = 0; i < 16; ++i, ++offset) {
            const wchar_t value = helios_native_ascii_lower(name[offset]);
            if (!((value >= L'0' && value <= L'9') || (value >= L'a' && value <= L'f')))
                return false;
        }
    }
    const wchar_t extension[] = L".dll";
    for (std::size_t i = 0; i < sizeof(extension) / sizeof(extension[0]); ++i)
        if (helios_native_ascii_lower(name[offset + i]) != extension[i]) return false;
    return true;
}
