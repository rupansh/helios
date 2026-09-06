#!/usr/bin/env python3
"""Distribute/check the canonical producer C ABI for independent build mirrors."""
import argparse
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--check", action="store_true")
args = parser.parse_args()
root = Path(__file__).resolve().parent.parent
source = (root / "protocol/include/helios_producer.h").read_bytes()
targets = [
    "dxvk-helios/src/dxvk/helios_producer_abi.h",
    "icd/mesa/src/virtio/vulkan/helios_producer_abi.h",
    "vkd3d-proton-helios/libs/vkd3d/helios_producer_abi.h",
]
for name in targets:
    path = root / name
    if args.check:
        if not path.exists() or path.read_bytes() != source:
            raise SystemExit(f"producer ABI differs: {name}")
    else:
        path.write_bytes(source)
print("Producer ABI copies match" if args.check else "Producer ABI copies updated")
