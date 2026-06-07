# Memory: System-Class Refocus

Date: 2026-06-07

The active Helios direction is System-class KMDF + DeviceIoControl + Mesa Venus. The WDDM Display-Only Driver pivot is archived and should not drive new work unless the owner explicitly asks for a DOD/display experiment.

Primary reference: `SYSTEM_CLASS_REFOCUS_2026_06_07.md`.

Active priorities:

- restore the old System-class driver VM/device setup;
- benchmark offscreen Venus rendering and fence/submit latency;
- improve async submit, interrupt/DPC fence completion, and blob mapping performance;
- treat windowed/DOD/scanout presentation as later integration work.

Keep `kmd/src/dxgk.rs`, `.dod-vidpn-types.md`, `DISPLAY.md`, `PHASE7_DISPLAY_HANDOVER.md`, and `CODE43_HANDOFF_FOR_CODEX.md` as historical reference material only.
