/*
 * helios_win_compat.h — Helios out-of-tree compat shim for building Mesa's
 * `venus` Vulkan driver on Windows.
 *
 * This header is FORCE-INCLUDED into every Mesa translation unit via the meson
 * `c_args`/`cpp_args` flag (`-include` for gcc/clang, `/FI` for cl/clang-cl). It
 * carries shared WinBoat branding from metadata/helios.env as well as compiler
 * compatibility definitions. Mesa itself is the icd/mesa fork; its Helios
 * backend and Windows support are maintained there.
 *
 * The `__ASSEMBLER__` guard matters: meson force-includes this onto .S files too
 * (e.g. blake3 SIMD), where it must be completely inert.
 */
#ifndef HELIOS_WIN_COMPAT_H
#define HELIOS_WIN_COMPAT_H

#if defined(_WIN32) && !defined(__ASSEMBLER__)

#include "../../metadata/helios_branding.h"

/* (1) pid_t — venus vn_common.h uses it as the return type of vn_gettid()
 *     (which already has a DETECT_OS_WINDOWS arm returning GetCurrentThreadId()).
 *     mingw supplies pid_t via <sys/types.h>; UCRT (cl / clang-cl) does not. */
#if !defined(__MINGW32__)
typedef int pid_t;
#endif

/* (2) clang-cl ONLY: Mesa's src/util/u_atomic.h takes the MSVC-intrinsic path
 *     (clang-cl defines _MSC_VER) and calls the *lowercase* 64-bit interlocked
 *     intrinsics, which MSVC's <intrin.h> declares but clang-cl's does not.
 *     Alias them to the capitalized clang builtins. gcc never compiles this. */
#if defined(_MSC_VER) && defined(__clang__)
#include <intrin.h>
#ifdef __cplusplus
extern "C" {
#endif
__forceinline __int64 _interlockedexchangeadd64(__int64 volatile *a, __int64 v) { return _InterlockedExchangeAdd64(a, v); }
__forceinline __int64 _interlockedexchange64(__int64 volatile *t, __int64 v)    { return _InterlockedExchange64(t, v); }
__forceinline __int64 _interlockedincrement64(__int64 volatile *a)              { return _InterlockedIncrement64(a); }
__forceinline __int64 _interlockeddecrement64(__int64 volatile *a)              { return _InterlockedDecrement64(a); }
__forceinline __int64 _interlockedadd64(__int64 volatile *a, __int64 v)         { return _InterlockedExchangeAdd64(a, v) + v; }
__forceinline long    _interlockedadd(long volatile *a, long v)                 { return _InterlockedExchangeAdd(a, v) + v; }
#ifdef __cplusplus
}
#endif
#endif /* clang-cl */

/* (3) BOTH toolchains: venus vn_queue.c / vn_wsi.c call the Linux sync-file
 *     helpers sync_wait()/sync_valid_fd() from src/util/libsync.h, which is
 *     POSIX-only and NOT included on Windows (vn_common.h guards that include
 *     behind !DETECT_OS_WINDOWS), so they become implicit-decl errors under
 *     Mesa's -Werror=implicit-function-declaration. Helios produces no DRM
 *     sync-file FDs (VN_SYNC_TYPE_IMPORTED_SYNC_FD is dead code on this
 *     transport), so inert stubs let the TU compile and the build link.
 *     `!_LIBSYNC_H` so we never clash if libsync.h is somehow included.
 *
 *     ⚠️ PLACEHOLDER: once vn_renderer_helios.c is written, real fence waits
 *     route through the KMD WAIT_FENCE IOCTL — NOT through these stubs. */
#if !defined(_LIBSYNC_H)
#include <stdbool.h>
static inline bool sync_valid_fd(int fd) { (void)fd; return false; }
static inline int  sync_wait(int fd, int timeout) { (void)fd; (void)timeout; return 0; }
#endif

#endif /* _WIN32 && !__ASSEMBLER__ */
#endif /* HELIOS_WIN_COMPAT_H */
