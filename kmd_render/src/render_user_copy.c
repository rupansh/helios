/* SEH-guarded probe + copy of DxgkDdiRender's user-mode command buffer.
 *
 * `DXGKARG_RENDER::pCommand` is the context's command buffer, which stays
 * mapped and WRITABLE in the submitting process for the whole call. A bare
 * copy therefore has two faults available to any process: an unmapped page
 * (the producer freed or shrank the buffer between D3DKMTRender and this DDI)
 * and a kernel-address confusion. Both raise, and a raise unwinding out of a
 * no_std Rust DDI bugchecks — the same trap the retired broad user-copy
 * wrapper existed for, on a
 * different pointer.
 *
 * DXGKDDI_RENDER is _IRQL_requires_(PASSIVE_LEVEL) (d3dkmddi.h:160), which is
 * what makes ProbeForRead legal here: it requires IRQL <= APC_LEVEL.
 *
 * The probe and the copy get SEPARATE __try blocks and distinct return codes on
 * purpose. They fail for different reasons — a probe raise means the range is
 * not user-space at all, a copy raise means it was unmapped under us — and a
 * single "it faulted" counter could not tell the two apart on the target.
 *
 * Deliberately header-free, exactly as the retired wrapper was and for the same reason:
 * the cc build has no WDK include paths (find-msvc-tools synthesizes ucrt/um/
 * shared, never km) and _KERNEL_MODE is undefined, so <ntddk.h> does not
 * compile here. Types match the x64 WDK ABI: SIZE_T = unsigned __int64,
 * ULONG = unsigned long. ProbeForRead is a real NTKERNELAPI export (wdm.h:28150
 * declares it; there is no macro form in kit 28000), and both it and memcpy
 * resolve from ntoskrnl.lib, already on the link. __C_specific_handler likewise
 * — proven by the earlier bounded wrapper shipping.
 */

typedef void *PVOID;

void __stdcall ProbeForRead(const volatile void *Address, unsigned __int64 Length,
                            unsigned long Alignment);
void *memcpy(void *Destination, const void *Source, unsigned __int64 Length);

#define EXCEPTION_EXECUTE_HANDLER 1

#define HELIOS_RENDER_COPY_OK 0u
#define HELIOS_RENDER_COPY_PROBE_FAULTED 1u
#define HELIOS_RENDER_COPY_COPY_FAULTED 2u

/* Copy `Length` bytes of a user-mode command buffer into kernel memory.
 *
 * `Alignment` is passed to ProbeForRead unchanged; callers pass 1 because HNR2
 * reads its header and tables as unaligned bytes (dxgkrnl promises nothing
 * about the command buffer's alignment, which is why the Rust side uses
 * read_unaligned).
 *
 * A zero-length copy is OK without touching either pointer: ProbeForRead with
 * Length 0 is documented to do nothing, and memcpy of 0 from a null source is
 * UB in C even though it is a no-op in practice.
 */
unsigned int
helios_render_copy_user_seh(void *Destination, const void *Source, unsigned __int64 Length,
                            unsigned long Alignment)
{
    if (Length == 0)
        return HELIOS_RENDER_COPY_OK;

    __try {
        ProbeForRead(Source, Length, Alignment);
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        return HELIOS_RENDER_COPY_PROBE_FAULTED;
    }

    /* The copy stays inside its own __try because the probe is TOCTOU: another
     * thread in the submitting process may unmap the range between the two. */
    __try {
        memcpy(Destination, Source, Length);
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        return HELIOS_RENDER_COPY_COPY_FAULTED;
    }

    return HELIOS_RENDER_COPY_OK;
}
