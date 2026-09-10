#!/usr/bin/env python3
"""Compile the diagnostic's exact GLSL sources to C headers and SPIR-V."""
import argparse
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser()
parser.add_argument("output", type=Path)
parser.add_argument("--glslang", default="glslangValidator")
args = parser.parse_args()
args.output.mkdir(parents=True, exist_ok=True)
root = Path(__file__).resolve().parent
for name, source, defines in [
    ("depth_fullscreen", "fullscreen.vert", []),
    ("depth_write", "write.frag", []),
    ("depth_write_mixed", "write.frag", ["-DMIXED=1"]),
    ("depth_read", "read.comp", []),
    ("depth_read_ms", "read.comp", ["-DMS=1"]),
    ("depth_read_color", "read.comp", ["-DCOLOR=1"]),
]:
    command = [args.glslang, "-V", "--target-env", "vulkan1.2", *defines, str(root / source)]
    subprocess.run([*command, "--vn", name, "-o", str(args.output / (name + ".h"))], check=True)
    subprocess.run([*command, "-o", str(args.output / (name + ".spv"))], check=True)
