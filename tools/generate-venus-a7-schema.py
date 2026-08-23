#!/usr/bin/env python3
"""Generate the bounded Rust decoder for Mesa's record-only Venus stream.

The input is the exact venus-protocol generator checkout used to produce the
headers under icd/mesa.  The generated Rust never dispatches a command; it only
walks the wire shape, proves complete byte consumption, and reports the zeroed
host-resource operands which the KMD must patch in its private copy.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import types
from pathlib import Path


EXPECTED_GENERATOR_REV = "70991d4c"

OUTER_COMMANDS = {
    "vkAllocateMemory": "Allocation",
    "vkFreeMemory": "Allocation",
    "vkDestroyBuffer": "Allocation",
    "vkDestroyImage": "Allocation",
    "vkBindBufferMemory2": "Allocation",
    "vkBindImageMemory2": "Allocation",
    "vkBeginCommandBuffer": "CommandRecord",
    "vkEndCommandBuffer": "CommandRecord",
    "vkQueueSubmit": "Queue",
    "vkQueueSubmit2": "Queue",
    "vkQueueBindSparse": "Queue",
}

# These operations have a different owner than HVC1.  Everything else reached
# through a generated vn_call/vn_async edge in the selected record-only source
# is a bounded pure-control command and must be parsed before KMD forwarding.
CONTROL_EXCLUDED = set(OUTER_COMMANDS) | {
    "vkCreateInstance",          # K11 created the sole host instance
    "vkCreateRingMESA",
    "vkDestroyRingMESA",
    "vkNotifyRingMESA",
    "vkSubmitVirtqueueSeqnoMESA",
    "vkWaitRingSeqnoMESA",
    "vkWaitVirtqueueSeqnoMESA",
    "vkImportSemaphoreResourceMESA",
    "vkResetFenceResourceMESA",
    "vkWaitSemaphoreResourceMESA",
    "vkGetMemoryResourcePropertiesMESA",
}

CONTROL_EXCLUDED_FILES = {
    "vn_acceleration_structure.c",
    "vn_host_copy.c",
}

# These output arrays carry only their count in the request because each
# partial element is zero bytes.  Bind the count to the exact generated reply
# size instead of trying to consume nonexistent request elements.
#
# vkEnumerateDeviceExtensionProperties reply:
#   command/result + pPropertyCount + pProperties count = 28 bytes
#   VkExtensionProperties = array count + 256-byte name + specVersion = 268
ZERO_WIDTH_REPLY_ARRAYS = {
    ("vkEnumerateDeviceExtensionProperties", "pProperties"): (28, 268),
}

# VkWriteDescriptorSet's three descriptor payload pointers are a semantic
# union selected by descriptorType, but vk.xml represents them as independent
# noautovalidity arrays.  The Venus encoder therefore emits descriptorCount
# for a present pointer and zero for each absent pointer.  Keep this exception
# bound to the exact generated wire fields instead of weakening every
# noautovalidity array.
WIRE_NULLABLE_DYNAMIC_ARRAYS = {
    ("VkWriteDescriptorSet", "pImageInfo"),
    ("VkWriteDescriptorSet", "pBufferInfo"),
    ("VkWriteDescriptorSet", "pTexelBufferView"),
}

# Reply-bearing maintenance fallbacks may either create, query, and destroy one
# private unbound buffer or create and query one still-live image in a single
# HVC1 transaction.  Capture only the exact generated handles needed to prove
# that each bounded sequence names the same device/object; no general object
# lookup or heuristic handle class is added.
CAPTURED_COMMAND_HANDLES = {
    ("vkCreateBuffer", "device"),
    ("vkCreateBuffer", "pBuffer"),
    ("vkDestroyBuffer", "device"),
    ("vkDestroyBuffer", "buffer"),
    ("vkGetBufferMemoryRequirements2", "device"),
    ("vkCreateImage", "device"),
    ("vkCreateImage", "pImage"),
    ("vkGetImageMemoryRequirements2", "device"),
}

CAPTURED_STRUCT_HANDLES = {
    ("VkBufferMemoryRequirementsInfo2", "buffer"),
    ("VkImageMemoryRequirementsInfo2", "image"),
}


def command_buffer_commands(source: Path) -> set[str]:
    text = source.read_text(encoding="utf-8")
    names = set(re.findall(r"VN_CMD_ENQUEUE\(\s*(vk\w+)", text))
    names.update(("vkBeginCommandBuffer", "vkEndCommandBuffer"))
    return names


def control_commands(mesa: Path) -> set[str]:
    names: set[str] = set()
    root = mesa / "src/virtio/vulkan"
    for source in root.glob("*.c"):
        if source.name in CONTROL_EXCLUDED_FILES:
            continue
        text = source.read_text(encoding="utf-8")
        names.update(re.findall(r"vn_(?:call|async)_(vk\w+)", text))
    return {
        name
        for name in names
        if name not in CONTROL_EXCLUDED
        and not name.startswith("vkCreateRayTracing")
        and "AccelerationStructure" not in name
        and "RayTracing" not in name
    }


def stub_mako() -> None:
    """vn_protocol imports Mako even when only its schema model is used."""
    mako = types.ModuleType("mako")
    lookup = types.ModuleType("mako.lookup")
    template = types.ModuleType("mako.template")
    lookup.TemplateLookup = type(
        "TemplateLookup", (), {"__init__": lambda self, *args, **kwargs: None}
    )
    template.Template = type("Template", (), {})
    sys.modules.update(
        {"mako": mako, "mako.lookup": lookup, "mako.template": template}
    )


def snake(name: str) -> str:
    name = re.sub(r"([A-Z]+)([A-Z][a-z])", r"\1_\2", name)
    name = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", name)
    result = re.sub(r"[^A-Za-z0-9_]", "_", name).lower()
    if result in {
        "as", "break", "const", "continue", "crate", "else", "enum",
        "extern", "false", "fn", "for", "if", "impl", "in", "let",
        "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
        "self", "Self", "static", "struct", "super", "trait", "true",
        "type", "unsafe", "use", "where", "while", "async", "await",
        "dyn", "abstract", "become", "box", "do", "final", "macro",
        "override", "priv", "typeof", "unsized", "virtual", "yield", "try",
    }:
        result += "_"
    return result


class Generator:
    CONSTANTS = {
        "VK_UUID_SIZE": 16,
        "VK_LUID_SIZE": 8,
        "VK_MAX_PHYSICAL_DEVICE_NAME_SIZE": 256,
        "VK_MAX_EXTENSION_NAME_SIZE": 256,
        "VK_MAX_DESCRIPTION_SIZE": 256,
        "VK_MAX_DRIVER_NAME_SIZE": 256,
        "VK_MAX_DRIVER_INFO_SIZE": 256,
        "VK_MAX_MEMORY_TYPES": 32,
        "VK_MAX_MEMORY_HEAPS": 16,
        "VK_MAX_DEVICE_GROUP_SIZE": 32,
        "VK_MAX_GLOBAL_PRIORITY_SIZE": 16,
    }

    def __init__(self, vp, vk_type, gen, commands: dict[str, str]):
        self.vp = vp
        self.VkType = vk_type
        self.gen = gen
        self.reg = gen.reg
        self.commands = commands
        self.enum_values: dict[str, int] = dict(self.CONSTANTS)
        for ty in self.reg.type_table.values():
            if ty.category == vk_type.ENUM and ty.enums and ty.enums.values:
                for key, value in ty.enums.values.items():
                    try:
                        self.enum_values[key] = int(value, 0)
                    except (TypeError, ValueError):
                        pass
        self.struct_modes: set[tuple[str, str]] = set()
        self.union_names: set[str] = set()
        self.lines: list[str] = []

    def value(self, token: str) -> int:
        if token in self.enum_values:
            return self.enum_values[token]
        return int(token, 0)

    def scalar_width(self, ty) -> int | None:
        base = ty.base
        if base.category == self.VkType.DEFAULT:
            if base.name in self.gen.PRIMITIVE_TYPES:
                return max(4, self.gen.PRIMITIVE_TYPES[base.name])
            if base.name == "char":
                return 4
            if base.name == "size_t":
                return 8
            return None
        if base.category in (self.VkType.BASETYPE, self.VkType.BITMASK):
            return self.scalar_width(base.typedef) if base.typedef else None
        if base.category == self.VkType.ENUM:
            return 8 if base.enums.bitwidth == 64 else 4
        return None

    def is_scalar(self, ty) -> bool:
        return self.scalar_width(ty) is not None

    def parsed_name(self, ty) -> str:
        return f"Parsed{ty.name}"

    def parse_name(self, ty, mode: str) -> str:
        return f"parse_{mode}_{snake(ty.name)}"

    def parse_self_name(self, ty, mode: str) -> str:
        return f"parse_{mode}_{snake(ty.name)}_self"

    def pnext_name(self, ty, mode: str) -> str:
        return f"parse_{mode}_{snake(ty.name)}_pnext"

    def scalar_fields(self, ty) -> list:
        return [var for var in ty.variables if self.is_scalar(var.ty)]

    def parsed_fields(self, ty) -> list:
        return [
            var
            for var in ty.variables
            if self.is_scalar(var.ty)
            or (ty.name, var.name) in CAPTURED_STRUCT_HANDLES
        ]

    def rust_expr(self, expr: str, locals_: dict[str, str], loop_vars: set[str]) -> str:
        expr = expr.strip()
        if expr == "null-terminated":
            return "0"
        expr = re.sub(
            r"\(([A-Za-z_]\w*) \? \1\[i\]\.geometryCount : 0\)",
            r"scratch.geometry(i)?",
            expr,
        )
        expr = expr.replace("->", ".")
        expr = re.sub(
            r"\b([A-Za-z_]\w*)\[i\]\.geometryCount",
            r"scratch.geometry(i)?",
            expr,
        )
        # The generator inserts this null guard for output arrays whose length
        # lives in an optional input structure. A missing pointer has the
        # default Parsed* value, so the guard is already represented by zero.
        expr = re.sub(
            r"\(([A-Za-z_]\w*) \? \1\.([A-Za-z_]\w*) : 0\)",
            r"\1.\2",
            expr,
        )
        expr = re.sub(
            r"\(([A-Za-z_]\w*) \? \*\1 : 0\)",
            r"\1",
            expr,
        )
        for owner, local in locals_.items():
            expr = re.sub(
                rf"\b{re.escape(owner)}\.([A-Za-z_]\w*)",
                lambda match: f"{local}.{snake(match.group(1))}",
                expr,
            )
        for token in sorted(
            set(re.findall(r"(?<!\.)\b[A-Za-z_]\w*\b", expr)),
            key=len,
            reverse=True,
        ):
            if token in {"scratch", "geometry", "i"} or token in loop_vars:
                continue
            if token in self.enum_values:
                replacement = str(self.enum_values[token])
            elif token in locals_:
                replacement = locals_[token]
            elif token in locals_.values():
                replacement = token
            elif token in {"pInfos", "geometryCount"}:
                replacement = snake(token)
            else:
                raise RuntimeError(f"unsupported length token {token!r} in {expr!r}")
            expr = re.sub(rf"\b{re.escape(token)}\b", replacement, expr)
        expr = expr.replace(".geometryCount", ".geometry_count")
        return f"u64::try_from({expr}).map_err(|_| VenusReject::CountOverflow)?"

    def condition_expr(self, condition: str, locals_: dict[str, str]) -> str:
        condition = condition.replace("val->", "")
        flags = re.fullmatch(r"!\((\w+) & (\w+)\)", condition)
        if flags:
            lhs = locals_[flags.group(1)]
            rhs = self.value(flags.group(2))
            return f"({lhs} & {rhs}) == 0"
        equal = re.fullmatch(r"(\w+) == (\w+)", condition)
        if equal:
            return f"{locals_[equal.group(1)]} == {self.value(equal.group(2))}"
        raise RuntimeError(f"unsupported condition {condition!r}")

    def validity(self, owner, var, mode: str, command: bool = False) -> int:
        initialized = "var_in" in var.attrs if command else mode == "full"
        return self.gen._get_variable_validity(owner, var, initialized)

    def ensure_type(self, ty, mode: str) -> None:
        base = ty.base
        if base.category == self.VkType.STRUCT:
            self.struct_modes.add((base.name, mode))
        elif base.category == self.VkType.UNION:
            self.union_names.add(base.name)

    def emit_scalar_read(self, width: int, target: str | None, indent: str) -> list[str]:
        method = "u64" if width == 8 else "u32"
        if target:
            return [f"{indent}{target} = c.{method}()? as u64;"]
        return [f"{indent}c.skip({width})?;"]

    def emit_element(
        self,
        owner,
        var,
        validity: int,
        mode: str,
        locals_: dict[str, str],
        indent: str,
        capture: str | None = None,
    ) -> list[str]:
        base = var.ty.base
        width = self.scalar_width(var.ty)
        if width is not None:
            return self.emit_scalar_read(width, capture, indent)
        if base.category == self.VkType.HANDLE:
            if capture:
                return [f"{indent}{capture} = c.u64()? as u64;"]
            return [f"{indent}c.skip(8)?;"]
        if base.category == self.VkType.STRUCT:
            child_mode = "partial" if validity == self.gen.VariableInfo.PARTIAL else "full"
            self.ensure_type(base, child_mode)
            call = f"{self.parse_name(base, child_mode)}(c, operands, scratch, depth + 1)?"
            if capture:
                return [f"{indent}{capture} = {call};"]
            return [f"{indent}let _ = {call};"]
        if base.category == self.VkType.UNION:
            self.ensure_type(base, "full")
            selector = var.attrs.get("selector")
            arg = f", {locals_[selector]}" if selector else ""
            return [f"{indent}parse_{snake(base.name)}(c, operands, scratch, depth + 1{arg})?;"]
        if base.name == "void" and var.is_blob():
            raise RuntimeError("blob element must be emitted as a scalar array")
        raise RuntimeError(f"unsupported element {owner.name}.{var.name}: {base.name}")

    def emit_array(
        self,
        owner,
        var,
        validity: int,
        mode: str,
        locals_: dict[str, str],
        indent: str,
        command_name: str | None,
    ) -> list[str]:
        info = self.gen.VariableInfo(owner, var, "", validity)
        loops = list(info.loop_info)
        array_expr = info.array_size
        dims = [loop.iter_count for loop in loops]
        if array_expr:
            dims.append(array_expr)
        if not dims:
            raise RuntimeError(f"array without dimensions: {owner.name}.{var.name}")

        out: list[str] = []
        loop_vars: set[str] = set()

        def emit_dim(level: int, cur_indent: str) -> None:
            expr = dims[level]
            # Nested `char **` encodings carry their own array length and the
            # parser below also requires the final NUL.  There is deliberately
            # no host strlen over an untrusted wire pointer to compare against.
            unchecked_string = expr == "null-terminated" or "strlen(" in expr
            expected = "0" if unchecked_string else self.rust_expr(expr, locals_, loop_vars)
            count = f"count_{snake(var.name)}_{level}"
            out.append(f"{cur_indent}let {count} = c.array_count()?;")
            if not unchecked_string:
                optional = level == 0 and (
                    var.is_optional()
                    or (owner.name, var.name) in WIRE_NULLABLE_DYNAMIC_ARRAYS
                )
                if optional:
                    out.append(f"{cur_indent}if {count} != 0 && {count} != {expected} {{")
                else:
                    out.append(f"{cur_indent}if {count} != {expected} {{")
                out.append(f"{cur_indent}    return Err(VenusReject::BadArrayCount);")
                out.append(f"{cur_indent}}}")
            if level + 1 < len(dims):
                index = chr(ord("i") + level)
                out.append(f"{cur_indent}c.bound_loop_count({count})?;")
                out.append(f"{cur_indent}for {index} in 0..{count} {{")
                loop_vars.add(index)
                emit_dim(level + 1, cur_indent + "    ")
                loop_vars.remove(index)
                out.append(f"{cur_indent}}}")
                return

            base = var.ty.base
            width = self.scalar_width(var.ty)
            reply_shape = ZERO_WIDTH_REPLY_ARRAYS.get((command_name, var.name))
            if reply_shape is not None:
                base_bytes, element_bytes = reply_shape
                out.append(
                    f"{cur_indent}scratch.expect_fixed_reply_array("
                    f"{count}, {base_bytes}, {element_bytes})?;"
                )
                return
            if info.func_stem in ("blob_array", "char_array"):
                out.append(f"{cur_indent}let bytes = c.take_padded({count})?;")
                if info.func_stem == "char_array":
                    optional_string = var.is_optional() and level == 0
                    zero_check = (
                        f"{count} != 0 && "
                        if optional_string
                        else f"{count} == 0 || "
                    )
                    out.append(
                        f"{cur_indent}if {zero_check}bytes[{count} as usize - 1] != 0 {{"
                    )
                    out.append(f"{cur_indent}    return Err(VenusReject::BadArrayCount);")
                    out.append(f"{cur_indent}}}")
            elif width is not None and info.array_size:
                out.append(f"{cur_indent}c.skip_array({count}, {width})?;")
            else:
                index = chr(ord("i") + level)
                out.append(f"{cur_indent}c.bound_loop_count({count})?;")
                out.append(f"{cur_indent}for {index} in 0..{count} {{")
                capture = None
                if command_name in {
                    "vkCmdBuildAccelerationStructuresKHR",
                    "vkCmdBuildAccelerationStructuresIndirectKHR",
                } and var.name == "pInfos":
                    capture = "parsed_info"
                    out.append(f"{cur_indent}    let mut parsed_info = {self.parsed_name(base)}::default();")
                out.extend(
                    self.emit_element(
                        owner, var, validity, mode, locals_, cur_indent + "    ", capture
                    )
                )
                if capture:
                    out.append(f"{cur_indent}    scratch.push_geometry(parsed_info.geometry_count)?;")
                out.append(f"{cur_indent}}}")

        emit_dim(0, indent)
        return out

    def emit_var(
        self,
        owner,
        var,
        validity: int,
        mode: str,
        locals_: dict[str, str],
        indent: str,
        command_name: str | None = None,
    ) -> list[str]:
        if (
            var.name == "resourceId"
            and validity != self.gen.VariableInfo.INVALID
            and self.is_scalar(var.ty)
            and not var.ty.is_pointer()
            and not var.ty.is_static_array()
        ):
            width = self.scalar_width(var.ty)
            target = locals_.get(var.name)
            out = [
                f"{indent}let operand_offset = c.offset_u32()?;",
                f"{indent}let resource_id = c.u{width * 8}()? as u64;",
                f"{indent}if resource_id != 0 {{ return Err(VenusReject::NonZeroResourceOperand); }}",
                f"{indent}operands.push_resource(operand_offset, {width})?;",
            ]
            if target:
                out.append(f"{indent}{target} = 0;")
            return out

        if not self.gen.is_serializable(var):
            if not var.ty.is_pointer():
                return []
            return [
                f"{indent}if c.pointer()? {{",
                f"{indent}    return Err(VenusReject::UnsupportedChain);",
                f"{indent}}}",
            ]

        if validity == self.gen.VariableInfo.INVALID:
            if var.is_dynamic_array():
                return [f"{indent}let _ = c.array_count()?; /* output pointer only */"]
            if var.ty.is_pointer():
                return [f"{indent}let _ = c.pointer()?; /* output pointer only */"]
            return []

        if var.is_dynamic_array() or var.ty.is_static_array():
            if "condition" in var.attrs:
                condition = self.condition_expr(var.attrs["condition"], locals_)
                body = self.emit_array(owner, var, validity, mode, locals_, indent + "    ", command_name)
                return [f"{indent}if {condition} {{", *body, f"{indent}}} else if c.array_count()? != 0 {{", f"{indent}    return Err(VenusReject::BadArrayCount);", f"{indent}}}"]
            return self.emit_array(owner, var, validity, mode, locals_, indent, command_name)

        if var.ty.is_pointer():
            present = f"present_{snake(var.name)}"
            out = [f"{indent}let {present} = c.pointer()?;"]
            required = not var.is_optional() and var.can_validate()
            if required:
                out.append(f"{indent}if !{present} {{ return Err(VenusReject::BadPointer); }}")
            capture = locals_.get(var.name)
            out.append(f"{indent}if {present} {{")
            out.extend(self.emit_element(owner, var, validity, mode, locals_, indent + "    ", capture))
            out.append(f"{indent}}}")
            return out

        capture = locals_.get(var.name)
        return self.emit_element(owner, var, validity, mode, locals_, indent, capture)

    def emit_parsed_structs(self) -> list[str]:
        names = sorted({name for name, _ in self.struct_modes})
        out: list[str] = []
        for name in names:
            ty = self.reg.type_table[name]
            fields = self.parsed_fields(ty)
            out.append("#[derive(Clone, Copy, Debug, Default)]")
            out.append(f"struct {self.parsed_name(ty)} {{")
            for var in fields:
                out.append(f"    {snake(var.name)}: u64,")
            out.append("}")
            out.append("")
        return out

    def discover(self) -> None:
        pending = list(self.commands)
        for name in pending:
            command = self.reg.type_table[name]
            for var in command.variables:
                validity = self.validity(command, var, "full", command=True)
                if validity == self.gen.VariableInfo.INVALID:
                    continue
                self.ensure_type(var.ty, "partial" if validity == self.gen.VariableInfo.PARTIAL else "full")
        changed = True
        while changed:
            changed = False
            before = len(self.struct_modes) + len(self.union_names)
            for name, mode in list(self.struct_modes):
                ty = self.reg.type_table[name]
                for node in ty.p_next:
                    self.struct_modes.add((node.name, mode))
                for var in ty.variables[2 if ty.s_type else 0 :]:
                    validity = self.validity(ty, var, mode)
                    if validity == self.gen.VariableInfo.INVALID:
                        continue
                    self.ensure_type(var.ty, "partial" if validity == self.gen.VariableInfo.PARTIAL else "full")
            for name in list(self.union_names):
                ty = self.reg.type_table[name]
                for _, var in ty.get_union_cases():
                    self.ensure_type(var.ty, "full")
            changed = before != len(self.struct_modes) + len(self.union_names)

    def emit_pnext(self, ty, mode: str) -> list[str]:
        name = self.pnext_name(ty, mode)
        out = [
            f"fn {name}(c: &mut Cursor<'_>, operands: &mut OperandWriter<'_>, scratch: &mut SchemaScratch<'_>, depth: u32) -> Result<(), VenusReject> {{",
            "    if depth >= MAX_A7_NESTING { return Err(VenusReject::UnsupportedChain); }",
            "    if !c.pointer()? { return Ok(()); }",
            "    match c.u32()? {",
        ]
        seen: set[int] = set()
        for node in ty.p_next:
            if not node.s_type:
                continue
            stype = self.value(node.s_type)
            if stype in seen:
                continue
            seen.add(stype)
            self.struct_modes.add((node.name, mode))
            out.extend(
                [
                    f"        {stype} => {{",
                    f"            {name}(c, operands, scratch, depth + 1)?;",
                    f"            let _ = {self.parse_self_name(node, mode)}(c, operands, scratch, depth + 1)?;",
                    "            Ok(())",
                    "        }",
                ]
            )
        out.extend(["        _ => Err(VenusReject::UnsupportedChain),", "    }", "}", ""])
        return out

    def emit_struct(self, ty, mode: str) -> list[str]:
        fields = self.parsed_fields(ty)
        locals_ = {var.name: f"parsed.{snake(var.name)}" for var in fields}
        out: list[str] = [
            f"fn {self.parse_self_name(ty, mode)}(c: &mut Cursor<'_>, operands: &mut OperandWriter<'_>, scratch: &mut SchemaScratch<'_>, depth: u32) -> Result<{self.parsed_name(ty)}, VenusReject> {{",
            "    if depth >= MAX_A7_NESTING { return Err(VenusReject::UnsupportedChain); }",
            f"    let mut parsed = {self.parsed_name(ty)}::default();",
        ]
        skip = 2 if ty.s_type else 0
        for var in ty.variables[skip:]:
            validity = self.validity(ty, var, mode)
            out.extend(self.emit_var(ty, var, validity, mode, locals_, "    "))
        out.extend(["    Ok(parsed)", "}", ""])

        out.extend(
            [
                f"fn {self.parse_name(ty, mode)}(c: &mut Cursor<'_>, operands: &mut OperandWriter<'_>, scratch: &mut SchemaScratch<'_>, depth: u32) -> Result<{self.parsed_name(ty)}, VenusReject> {{",
                "    if depth >= MAX_A7_NESTING { return Err(VenusReject::UnsupportedChain); }",
            ]
        )
        if ty.s_type:
            out.extend(
                [
                    f"    if c.u32()? != {self.value(ty.s_type)} {{ return Err(VenusReject::BadStructureType); }}",
                    f"    {self.pnext_name(ty, mode)}(c, operands, scratch, depth + 1)?;",
                ]
            )
        out.extend(
            [
                f"    {self.parse_self_name(ty, mode)}(c, operands, scratch, depth + 1)",
                "}",
                "",
            ]
        )
        if ty.s_type:
            out.extend(self.emit_pnext(ty, mode))
        return out

    def emit_union(self, ty) -> list[str]:
        args = ", selector: u64" if ty.is_valid_union() else ""
        out = [
            f"fn parse_{snake(ty.name)}(c: &mut Cursor<'_>, operands: &mut OperandWriter<'_>, scratch: &mut SchemaScratch<'_>, depth: u32{args}) -> Result<(), VenusReject> {{",
            "    if depth >= MAX_A7_NESTING { return Err(VenusReject::UnsupportedChain); }",
            "    let tag = c.u32()? as u64;",
        ]
        if ty.is_valid_union():
            out.append("    if tag != selector { return Err(VenusReject::BadStructureType); }")
        else:
            out.append(f"    if tag != {self.gen.UNION_DEFAULT_TAGS[ty.name]} {{ return Err(VenusReject::BadStructureType); }}")
        out.append("    match tag {")
        seen: set[int] = set()
        fixed_tag = None if ty.is_valid_union() else self.gen.UNION_DEFAULT_TAGS[ty.name]
        for case, var in ty.get_union_cases():
            value = self.value(case) if isinstance(case, str) else int(case)
            if fixed_tag is not None and value != fixed_tag:
                continue
            if value in seen:
                continue
            seen.add(value)
            out.append(f"        {value} => {{")
            # Union members retain the same generated wire shape as ordinary
            # variables.  In particular, fixed-size scalar arrays carry an
            # explicit array count before their elements; treating the member
            # as one scalar under-consumes the command that follows it.
            out.extend(
                self.emit_var(
                    ty,
                    var,
                    self.gen.VariableInfo.VALID,
                    "full",
                    {},
                    "            ",
                )
            )
            out.append("            Ok(())")
            out.append("        }")
        out.extend(["        _ => Err(VenusReject::BadStructureType),", "    }", "}", ""])
        return out

    def emit_memory_properties2_command(self, name: str, kind: str) -> list[str]:
        """Emit the output-only fixed-array shape used by this command.

        Venus writes only the two fixed array counts into the request because
        VkMemoryType_partial and VkMemoryHeap_partial are both zero bytes.  The
        ordinary recursive array walker cannot distinguish that from missing
        request bytes, so bind the counts and supported pNext shape directly to
        the exact reply sizes generated by vn_protocol_driver_device.h.
        """
        ty = self.reg.type_table[name]
        opcode = self.value(ty.attrs["c_type"])
        properties_stype = self.value(
            "VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MEMORY_PROPERTIES_2"
        )
        budget_stype = self.value(
            "VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MEMORY_BUDGET_PROPERTIES_EXT"
        )
        memory_types = self.value("VK_MAX_MEMORY_TYPES")
        memory_heaps = self.value("VK_MAX_MEMORY_HEAPS")

        # command type + output pointer + outer sType/pNext + the full fixed
        # VkPhysicalDeviceMemoryProperties reply.
        null_reply_size = (
            4
            + 8
            + 4
            + 8
            + 4
            + 8
            + memory_types * (4 + 4)
            + 4
            + 8
            + memory_heaps * (8 + 4)
        )
        # Replace the null outer pNext pointer with the sole supported budget
        # node: pointer + sType + terminal pNext + two fixed VkDeviceSize arrays.
        budget_node_size = 8 + 4 + 8 + 2 * (8 + memory_heaps * 8)
        budget_reply_size = null_reply_size - 8 + budget_node_size

        return [
            f"fn parse_command_{snake(name)}(c: &mut Cursor<'_>, operands: &mut OperandWriter<'_>, scratch: &mut SchemaScratch<'_>) -> Result<A7CommandFacts, VenusReject> {{",
            "    c.skip(8)?; // physicalDevice",
            "    if !c.pointer()? { return Err(VenusReject::BadPointer); }",
            f"    if c.u32()? != {properties_stype} {{ return Err(VenusReject::BadStructureType); }}",
            "    let memory_budget = if c.pointer()? {",
            f"        if c.u32()? != {budget_stype} {{ return Err(VenusReject::UnsupportedChain); }}",
            "        if c.pointer()? {",
            "            let _ = c.u32()?;",
            "            return Err(VenusReject::UnsupportedChain);",
            "        }",
            "        true",
            "    } else {",
            "        false",
            "    };",
            "    let memory_type_slots = c.array_count()?;",
            f"    if memory_type_slots != {memory_types} {{ return Err(VenusReject::BadArrayCount); }}",
            "    let memory_heap_slots = c.array_count()?;",
            f"    if memory_heap_slots != {memory_heaps} {{ return Err(VenusReject::BadArrayCount); }}",
            f"    scratch.expect_reply_size(if memory_budget {{ {budget_reply_size} }} else {{ {null_reply_size} }})?;",
            "    Ok(A7CommandFacts {",
            f"        kind: A7CommandKind::{kind},",
            f"        opcode: {opcode},",
            "        allocation_size: 0,",
            "        memory_type_index: 0,",
            "    })",
            "}",
            "",
        ]

    def emit_command(self, name: str, kind: str) -> list[str]:
        if name == "vkGetPhysicalDeviceMemoryProperties2":
            return self.emit_memory_properties2_command(name, kind)

        ty = self.reg.type_table[name]
        opcode = self.value(ty.attrs["c_type"])
        locals_: dict[str, str] = {}
        declarations: list[str] = []
        for var in ty.variables:
            validity = self.validity(ty, var, "full", command=True)
            if self.is_scalar(var.ty):
                local = snake(var.name)
                if local == "depth":
                    local = "depth_arg"
                locals_[var.name] = local
                declarations.append(f"    let mut {local}: u64 = 0;")
            elif var.ty.base.category == self.VkType.STRUCT and var.ty.is_pointer():
                child_mode = "partial" if validity == self.gen.VariableInfo.PARTIAL else "full"
                self.ensure_type(var.ty, child_mode)
                local = snake(var.name)
                locals_[var.name] = local
                declarations.append(
                    f"    let mut {local} = {self.parsed_name(var.ty.base)}::default();"
                )
            elif (
                var.ty.base.category == self.VkType.HANDLE
                and (name, var.name) in CAPTURED_COMMAND_HANDLES
            ):
                local = snake(var.name)
                locals_[var.name] = local
                declarations.append(f"    let mut {local}: u64 = 0;")
        out = [
            f"fn parse_command_{snake(name)}(c: &mut Cursor<'_>, operands: &mut OperandWriter<'_>, scratch: &mut SchemaScratch<'_>) -> Result<A7CommandFacts, VenusReject> {{",
            "    let depth = 0u32;",
            *declarations,
        ]
        if name in {
            "vkCmdBuildAccelerationStructuresKHR",
            "vkCmdBuildAccelerationStructuresIndirectKHR",
        }:
            out.append("    scratch.reset_geometry();")
        for var in ty.variables:
            validity = self.validity(ty, var, "full", command=True)
            out.extend(self.emit_var(ty, var, validity, "full", locals_, "    ", name))
        identity = {
            "vkCreateBuffer": ("device", "p_buffer"),
            "vkDestroyBuffer": ("device", "buffer"),
            "vkGetBufferMemoryRequirements2": ("device", "p_info.buffer"),
            "vkCreateImage": ("device", "p_image"),
            "vkGetImageMemoryRequirements2": ("device", "p_info.image"),
        }.get(name)
        if identity:
            out.append(
                f"    scratch.set_command_identity({identity[0]}, {identity[1]});"
            )
        out.extend(
            [
                "    Ok(A7CommandFacts {",
                f"        kind: A7CommandKind::{kind},",
                f"        opcode: {opcode},",
                f"        allocation_size: {locals_.get('pAllocateInfo', 'Default::default()')}.allocation_size," if name == "vkAllocateMemory" else "        allocation_size: 0,",
                f"        memory_type_index: {locals_.get('pAllocateInfo', 'Default::default()')}.memory_type_index as u32," if name == "vkAllocateMemory" else "        memory_type_index: 0,",
                "    })",
                "}",
                "",
            ]
        )
        return out

    def generate(self, revision: str) -> str:
        self.discover()
        # Discovery must reach a fixed point before Parsed* definitions are emitted.
        self.discover()
        out = [
            "//! @generated by tools/generate-venus-a7-schema.py; do not hand edit.",
            f"//! venus-protocol revision: {revision}",
            "#![allow(dead_code, unused_assignments, unused_mut, unused_variables)]",
            "",
            "use super::{Cursor, OperandWriter, VenusReject};",
            "",
            "const MAX_A7_NESTING: u32 = 128;",
            "",
            "#[derive(Clone, Copy, Debug, PartialEq, Eq)]",
            "pub enum A7CommandKind { Allocation, CommandRecord, Queue, PureControl }",
            "",
            "#[derive(Clone, Copy, Debug, PartialEq, Eq)]",
            "pub struct A7CommandFacts {",
            "    pub kind: A7CommandKind,",
            "    pub opcode: u32,",
            "    pub allocation_size: u64,",
            "    pub memory_type_index: u32,",
            "}",
            "",
            "pub struct SchemaScratch<'a> {",
            "    geometry_counts: &'a mut [u32],",
            "    geometry_len: usize,",
            "    reply_size: u64,",
            "    command_device_handle: u64,",
            "    command_object_handle: u64,",
            "}",
            "",
            "impl<'a> SchemaScratch<'a> {",
            "    pub fn new(geometry_counts: &'a mut [u32]) -> Self { Self { geometry_counts, geometry_len: 0, reply_size: 0, command_device_handle: 0, command_object_handle: 0 } }",
            "    pub(super) fn set_reply_size(&mut self, reply_size: u64) { self.reply_size = reply_size; }",
            "    pub(super) fn begin_command(&mut self) { self.command_device_handle = 0; self.command_object_handle = 0; }",
            "    fn set_command_identity(&mut self, device: u64, object: u64) { self.command_device_handle = device; self.command_object_handle = object; }",
            "    pub(super) fn command_identity(&self) -> (u64, u64) { (self.command_device_handle, self.command_object_handle) }",
            "    fn expect_reply_size(&self, expected: u64) -> Result<(), VenusReject> {",
            "        if self.reply_size != expected { return Err(VenusReject::BadArrayCount); }",
            "        Ok(())",
            "    }",
            "    fn expect_fixed_reply_array(&self, count: u64, base_bytes: u64, element_bytes: u64) -> Result<(), VenusReject> {",
            "        let expected = count.checked_mul(element_bytes).and_then(|bytes| base_bytes.checked_add(bytes)).ok_or(VenusReject::CountOverflow)?;",
            "        self.expect_reply_size(expected)",
            "    }",
            "    fn reset_geometry(&mut self) { self.geometry_len = 0; }",
            "    fn push_geometry(&mut self, value: u64) -> Result<(), VenusReject> {",
            "        let slot = self.geometry_counts.get_mut(self.geometry_len).ok_or(VenusReject::OperandCapacity)?;",
            "        *slot = u32::try_from(value).map_err(|_| VenusReject::CountOverflow)?;",
            "        self.geometry_len += 1;",
            "        Ok(())",
            "    }",
            "    fn geometry(&self, index: u64) -> Result<u64, VenusReject> {",
            "        let index = usize::try_from(index).map_err(|_| VenusReject::CountOverflow)?;",
            "        self.geometry_counts.get(index).copied().map(u64::from).ok_or(VenusReject::BadArrayCount)",
            "    }",
            "}",
            "",
        ]
        out.extend(self.emit_parsed_structs())
        for name, mode in sorted(self.struct_modes):
            out.extend(self.emit_struct(self.reg.type_table[name], mode))
        for name in sorted(self.union_names):
            out.extend(self.emit_union(self.reg.type_table[name]))
        for name, kind in sorted(self.commands.items()):
            out.extend(self.emit_command(name, kind))
        out.extend(
            [
                "pub fn parse_a7_command(opcode: u32, flags: u32, c: &mut Cursor<'_>, operands: &mut OperandWriter<'_>, scratch: &mut SchemaScratch<'_>) -> Result<A7CommandFacts, VenusReject> {",
                "    if flags & !1 != 0 { return Err(VenusReject::BadFlags); }",
                "    scratch.begin_command();",
                "    match opcode {",
            ]
        )
        seen: set[int] = set()
        for name in sorted(self.commands):
            ty = self.reg.type_table[name]
            opcode = self.value(ty.attrs["c_type"])
            if opcode in seen:
                continue
            seen.add(opcode)
            out.append(f"        {opcode} => parse_command_{snake(name)}(c, operands, scratch),")
        out.extend(["        _ => Err(VenusReject::UnknownOpcode),", "    }", "}", ""])
        return "\n".join(out)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--venus-protocol", type=Path, required=True)
    parser.add_argument("--mesa", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()

    revision = subprocess.check_output(
        ["git", "-C", str(args.venus_protocol), "rev-parse", "--short=8", "HEAD"],
        text=True,
    ).strip()
    if revision != EXPECTED_GENERATOR_REV:
        raise SystemExit(
            f"venus-protocol revision {revision}, expected {EXPECTED_GENERATOR_REV}"
        )

    stub_mako()
    sys.path.insert(0, str(args.venus_protocol))
    import vn_protocol as vp
    from vkxml import VkRegistry, VkType

    registry = VkRegistry.parse(vp.VN_PROTOCOL_VK_XML, vp.VN_PROTOCOL_PRIVATE_XMLS)
    model = vp.Gen(True, registry)
    commands = {
        name: "CommandRecord"
        for name in command_buffer_commands(
            args.mesa / "src/virtio/vulkan/vn_command_buffer.c"
        )
    }
    commands.update(OUTER_COMMANDS)
    for name in control_commands(args.mesa):
        commands.setdefault(name, "PureControl")
    generated = Generator(vp, VkType, model, commands).generate(revision)
    args.output.write_text(generated, encoding="utf-8")
    # The checked-in decoder is reviewable Rust, not a formatter-dependent
    # by-product.  Generation and regeneration therefore end in the same
    # canonical formatting step used by the Rust crates.
    subprocess.run(["rustfmt", "--edition", "2021", str(args.output)], check=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
