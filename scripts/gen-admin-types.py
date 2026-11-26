#!/usr/bin/env python3
"""Renders the JSON Schema of the admin site config (the `admin-schema` binary) to the Python and TypeScript config types.

    python3 scripts/gen-admin-types.py                 # rewrite both generated files
    python3 scripts/gen-admin-types.py --python P --typescript T [--schema FILE]

The Rust structs of `crates/admin/src/site_config.rs` are the one source. Three schema extensions steer the output:
`x-monorm-type` names the user-facing type of a field whose user shape differs from the Rust type (`TYPES`; a host
position when the name is in `HOST_KINDS`), `x-monorm-internal` keeps a key out of the user types, and `enum` /
`items` narrow a string to its literals.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PYTHON_OUT = ROOT / "languages/python/monorm/_admin_types.py"
TYPESCRIPT_OUT = ROOT / "languages/js/src/admin-types.ts"

# The public name of each generated type, by Rust struct, as (Python, TypeScript).
NAMES: dict[str, tuple[str, str]] = {
    "AggregateConfig": ("AggregateColumn", "AggregateColumn"),
    "BrandConfig": ("BrandConfig", "BrandConfig"),
    "ComputedConfig": ("ComputedColumn", "ComputedColumn"),
    "DateHierarchyFull": ("DateHierarchy", "DateHierarchy"),
    "EmbedConfig": ("EmbedConfig", "EmbedConfig"),
    "Fieldset": ("Fieldset", "Fieldset"),
    "FrameConfig": ("FrameConfig", "FrameConfig"),
    "LinkConfig": ("BrandLink", "BrandLink"),
    "ListConfig": ("ListConfig", "ListConfig"),
    "NamedChoiceConfig": ("NamedChoice", "NamedChoice"),
    "NamedFilterConfig": ("NamedFilter", "NamedFilter"),
    "RelationConfig": ("RelationConfig", "RelationConfig"),
    "ResourceConfig": ("ResourceConfig", "ResourceConfig"),
    "SectionConfig": ("SectionConfig", "FormSection"),
    "SwitcherBuild": ("SwitcherBuild", "SwitcherBuild"),
    "SwitcherConfig": ("SwitcherConfig", "SwitcherConfig"),
    "SwitcherService": ("SwitcherService", "SwitcherService"),
    "TabOptions": ("PackageTabConfig", "PackageTabConfig"),
    "ThemeConfig": ("ThemeConfig", "ThemeConfig"),
}

# What a host position holds, by `x-monorm-type`. The bindings send each one through their `host` function.
HOST_KINDS: dict[str, str] = {
    "banners_function": "function",
    "batch_function": "function",
    "can_rules": "ir",
    "column": "column",
    "column_or_path": "column",
    "columns": "column",
    "columns_ref": "column",
    "condition": "ir",
    "date_hierarchy": "column",
    "editable": "column",
    "filters": "column",
    "ir": "ir",
    "order": "order",
    "order_by": "order_by",
    "prepared": "queryset",
    "queryset": "queryset",
    "relation": "relation",
    "relation_paths": "column",
    "search_with": "function",
}
HOST_KIND_NAMES = ["column", "relation", "order", "order_by", "ir", "queryset", "function"]

BADGE_COLOURS = ["grey", "blue", "green", "yellow", "red", "purple"]

# The user-facing type of an `x-monorm-type`, by language. `flatten` has no entry: `fields()` inlines the keys of
# the struct into the parent.
PY_TYPES: dict[str, str] = {
    "actions": "Sequence[Any]",
    "badges": "Mapping[ColumnRef[Any] | str, Mapping[str | int | bool, BadgeColour]]",
    "banners_function": "Callable[[dict[str, Any]], Awaitable[Sequence[Mapping[str, str]]]]",
    "batch_function": "Callable[[list[Any]], Awaitable[Sequence[Any]]]",
    "brand_colors": 'Mapping[Literal["primary", "accent"], str]',
    "can_rules": 'Mapping[Literal["change", "delete"], Any]',
    "column": "Any",
    "column_map": "Mapping[ColumnRef[Any] | str, str]",
    "column_or_path": "ColumnRef[Any] | str",
    "columns": "Sequence[ColumnRef[Any] | str]",
    "columns_ref": "Sequence[ColumnRef[Any]]",
    "condition": "Any",
    "date_hierarchy": "ColumnRef[Any] | str | DateHierarchy",
    "editable": "Sequence[ColumnRef[Any] | str]",
    "files": "Mapping[str, str]",
    "filters": "Sequence[ColumnRef[Any] | RelationPath[Any]]",
    "form": 'Sequence[SectionConfig] | Mapping[Literal["create", "edit"], Sequence[SectionConfig]]',
    "form_component": "str | os.PathLike[str]",
    "inlines": "Sequence[Any]",
    "inputs": "Sequence[InputDef]",
    "ir": "Any",
    "order": "Sequence[ColumnRef[Any] | Ordering]",
    "order_by": "Any",
    "prepared": "Any",
    "queryset": "Any",
    "relation": "Any",
    "relation_paths": "Sequence[RelationPath[Any]]",
    "search_with": "Any",
    "service_create": "Callable[..., Any]",
    "service_delete": "Callable[..., Any]",
    "service_update": "Callable[..., Any]",
}
TS_TYPES: dict[str, str] = {
    "actions": "readonly (string | ActionFunction)[]",
    "badges": "Readonly<Record<string, Readonly<Record<string, BadgeColour>>>>",
    "banners_function": "BannersFunction",
    "batch_function": "BatchFunction",
    "brand_colors": "{ readonly primary?: string; readonly accent?: string }",
    "can_rules": "CanRules",
    "column": "AnyColumn",
    "column_map": "Readonly<Record<string, string>>",
    "column_or_path": "AnyColumn | string",
    "columns": "readonly (AnyColumn | string)[]",
    "columns_ref": "readonly AnyColumn[]",
    "condition": "unknown",
    "date_hierarchy": "AnyColumn | string | DateHierarchy",
    "editable": "readonly AnyColumn[]",
    "files": "Readonly<Record<string, string>>",
    "filters": "readonly (AnyColumn | AnyRelation)[]",
    "form": "readonly FormSection[] | { readonly create?: readonly FormSection[]; readonly edit?: readonly FormSection[] }",
    "form_component": "string",
    "inlines": "readonly (InlineDef | AnyModel)[]",
    "inputs": "readonly InputDef[]",
    "ir": "unknown",
    "order": "readonly (AnyColumn | Ordering<any, any>)[]",
    "order_by": "unknown",
    "prepared": "Prepared<any, any, any>",
    "queryset": "Prepared<any, any, any> | QuerysetFunction",
    "relation": "AnyRelation",
    "relation_paths": "readonly AnyRelation[]",
    "search_with": "SearchWith",
    "service_create": "(ctx: Context, values: any) => Promise<unknown>",
    "service_delete": "(ctx: Context, qs: any) => Promise<unknown>",
    "service_update": "(ctx: Context, qs: any, changes: any) => Promise<unknown>",
}


def camel(name: str) -> str:
    head, *rest = name.split("_")
    return head + "".join(part.capitalize() for part in rest)


def ref_name(node: dict) -> str:
    return node["$ref"].rsplit("/", 1)[1]


def deref(schema: dict, node: dict) -> dict:
    return schema["$defs"][ref_name(node)] if "$ref" in node else node


def strip_null(node: dict) -> dict:
    """`Option<T>` is `anyOf [T, null]` or `type [T, null]`: the key is optional, so the null goes."""
    if "anyOf" in node:
        rest = [n for n in node["anyOf"] if n != {"type": "null"}]
        if len(rest) == 1:
            return {**{k: v for k, v in node.items() if k != "anyOf"}, **rest[0]}
        return {**node, "anyOf": rest}
    kind = node.get("type")
    if isinstance(kind, list):
        rest = [k for k in kind if k != "null"]
        return {**node, "type": rest[0] if len(rest) == 1 else rest}
    return node


class Lang:
    """One target language: how a schema node becomes a type expression."""

    def __init__(self, index: int) -> None:
        self.index = index

    def public(self, rust: str) -> str:
        return NAMES[rust][self.index]

    def user(self, name: str) -> str:
        raise NotImplementedError

    def union(self, parts: list[str]) -> str:
        raise NotImplementedError

    def literals(self, values: list[str]) -> str:
        raise NotImplementedError

    def string(self) -> str:
        raise NotImplementedError

    def integer(self) -> str:
        raise NotImplementedError

    def number(self) -> str:
        raise NotImplementedError

    def boolean(self) -> str:
        raise NotImplementedError

    def any(self) -> str:
        raise NotImplementedError

    def sequence(self, item: str) -> str:
        raise NotImplementedError

    def mapping(self, value: str) -> str:
        raise NotImplementedError

    def type_of(self, node: dict, name: str) -> str:
        if "x-monorm-type" in node:
            return self.user(node["x-monorm-type"])
        node = strip_null(node)
        if "$ref" in node:
            rust = ref_name(node)
            if rust not in NAMES:
                raise SystemExit(f"{name}: {rust} has no public name; tag the field with x-monorm-type or add it to NAMES")
            return self.public(rust)
        if "anyOf" in node:
            return self.union([self.type_of(n, name) for n in node["anyOf"]])
        kind = node.get("type")
        if "enum" in node:
            return self.literals(node["enum"])
        if kind == "string":
            return self.string()
        if kind == "integer":
            return self.integer()
        if kind == "number":
            return self.number()
        if kind == "boolean":
            return self.boolean()
        if kind == "array":
            return self.sequence(self.type_of(node.get("items", {}), name))
        if kind == "object":
            extra = node.get("additionalProperties", True)
            return self.mapping(self.any() if extra is True else self.type_of(extra, name))
        if kind is None:
            return self.any()
        raise SystemExit(f"{name}: no type for {node}")


class Python(Lang):
    def __init__(self) -> None:
        super().__init__(0)

    def user(self, name: str) -> str:
        return PY_TYPES[name]

    def union(self, parts: list[str]) -> str:
        return " | ".join(parts)

    def literals(self, values: list[str]) -> str:
        return "Literal[" + ", ".join(json.dumps(v) for v in values) + "]"

    def string(self) -> str:
        return "str"

    def integer(self) -> str:
        return "int"

    def number(self) -> str:
        return "float"

    def boolean(self) -> str:
        return "bool"

    def any(self) -> str:
        return "Any"

    def sequence(self, item: str) -> str:
        return f"Sequence[{item}]"

    def mapping(self, value: str) -> str:
        return f"Mapping[str, {value}]"

    def key(self, name: str) -> str:
        return name

    def doc(self, text: str, indent: str) -> list[str]:
        text = re.sub(r"(?<!`)`([^`]+)`(?!`)", r"``\1``", text.replace("\\", "\\\\"))
        if text.endswith('"'):
            raise SystemExit(f"a doc comment ends with a double quote: {text}")
        return [f'{indent}"""{text}"""']


class TypeScript(Lang):
    def __init__(self) -> None:
        super().__init__(1)

    def user(self, name: str) -> str:
        return TS_TYPES[name]

    def union(self, parts: list[str]) -> str:
        return " | ".join(parts)

    def literals(self, values: list[str]) -> str:
        return " | ".join(json.dumps(v) for v in values)

    def string(self) -> str:
        return "string"

    def integer(self) -> str:
        return "number"

    number = integer

    def boolean(self) -> str:
        return "boolean"

    def any(self) -> str:
        return "unknown"

    def sequence(self, item: str) -> str:
        return f"readonly ({item})[]" if re.search(r"[ |]", item) else f"readonly {item}[]"

    def mapping(self, value: str) -> str:
        return f"Readonly<Record<string, {value}>>"

    def key(self, name: str) -> str:
        return camel(name)

    def doc(self, text: str, indent: str) -> list[str]:
        return [f"{indent}/** {text} */"]


def fields(schema: dict, struct: dict, path: str = "") -> list[tuple[str, dict, bool]]:
    """The user keys of a struct as (name, node, required), sorted: `flatten` inlines a nested struct, `x-monorm-internal` drops a key."""
    out: list[tuple[str, dict, bool]] = []
    required = set(struct.get("required", []))
    for name, node in struct.get("properties", {}).items():
        if node.get("x-monorm-internal"):
            continue
        if node.get("x-monorm-type") == "flatten":
            inner = deref(schema, strip_null(node))
            out.extend((n, p, False) for n, p, _ in fields(schema, inner))
            continue
        out.append((name, node, name in required))
    return sorted(out, key=lambda item: item[0])


def render_python(schema: dict) -> str:
    lang = Python()
    lines = [
        '"""The admin config types, generated by scripts/gen-admin-types.py from the Rust structs of crates/admin/src/site_config.rs. Do not edit."""',
        "",
        "from __future__ import annotations",
        "",
        "import os",
        "from collections.abc import Awaitable, Callable",
        "from typing import Any, Literal, Mapping, NotRequired, Sequence, TypedDict",
        "",
        "from ._admin_write import InputDef",
        "from .expr import ColumnRef, Ordering, RelationPath",
        "",
    ]
    names = sorted(lang.public(rust) for rust in NAMES)
    lines += ["__all__ = [" + ", ".join(json.dumps(n) for n in ["BadgeColour", *names, "HOST_FIELDS"]) + "]", ""]
    lines += ["BadgeColour = Literal[" + ", ".join(json.dumps(c) for c in BADGE_COLOURS) + "]", ""]
    for rust in sorted(NAMES, key=lang.public):
        struct = schema["$defs"][rust]
        lines += ["", f"class {lang.public(rust)}(TypedDict):"]
        if struct.get("description"):
            lines += lang.doc(struct["description"], "    ") + [""]
        body = fields(schema, struct)
        for name, node, required in body:
            type_ = lang.type_of(node, f"{rust}.{name}")
            lines.append(f"    {lang.key(name)}: {type_ if required else f'NotRequired[{type_}]'}")
            if node.get("description"):
                lines += lang.doc(node["description"], "    ")
        if not body:
            lines.append("    pass")
        lines.append("")
    lines += ["", "# The config positions that hold a host object, and what each holds. The call sites send each one through `_host`.", "HOST_FIELDS: dict[str, str] = {"]
    lines += [f'    "{path}": "{kind}",' for path, kind in host_fields(schema, 0)]
    lines += ["}", ""]
    return "\n".join(lines)


def render_typescript(schema: dict) -> str:
    lang = TypeScript()
    lines = [
        "// The admin config types, generated by scripts/gen-admin-types.py from the Rust structs of crates/admin/src/site_config.rs. Do not edit.",
        "",
        'import type { ActionFunction, Context, InputDef } from "./admin-write.js";',
        'import type { BannersFunction, BatchFunction, CanRules, InlineDef, QuerysetFunction, SearchWith } from "./admin.js";',
        'import type { Column, Ordering, RelationPath } from "./expr.js";',
        'import type { ModelSpec } from "./meta.js";',
        'import type { ModelClass } from "./model.js";',
        'import type { Prepared } from "./query.js";',
        "",
        "type AnyColumn = Column<any, any, any, any>;",
        "type AnyModel = ModelClass<ModelSpec>;",
        "type AnyRelation = RelationPath<any, any, any>;",
        "",
        "export type BadgeColour = " + " | ".join(json.dumps(c) for c in BADGE_COLOURS) + ";",
    ]
    for rust in sorted(NAMES, key=lang.public):
        struct = schema["$defs"][rust]
        lines.append("")
        if struct.get("description"):
            lines += lang.doc(struct["description"], "")
        lines.append(f"export interface {lang.public(rust)} {{")
        for name, node, required in fields(schema, struct):
            if node.get("description"):
                lines += lang.doc(node["description"], "  ")
            lines.append(f"  readonly {lang.key(name)}{'' if required else '?'}: {lang.type_of(node, f'{rust}.{name}')};")
        lines.append("}")
    kinds = " | ".join(f'"{k}"' for k in HOST_KIND_NAMES)
    lines += ["", "/** The config positions that hold a host object, and what each holds. The call sites send each one through `hostJson`. */", f"export const HOST_FIELDS: Readonly<Record<string, {kinds}>> = {{"]
    lines += [f'  "{path}": "{kind}",' for path, kind in host_fields(schema, 1)]
    lines += ["};", ""]
    return "\n".join(lines)


def host_fields(schema: dict, index: int) -> list[tuple[str, str]]:
    """The host positions below `ResourceConfig`, with page-relative paths. Arrays and maps add no path segment."""
    found: dict[str, str] = {}

    def structs(node: dict) -> list[tuple[str, dict]]:
        """The named object types under a field: through `Option`, arrays, maps, `anyOf` and `$ref`."""
        node = strip_null(node)
        if "$ref" in node:
            name = ref_name(node)
            target = schema["$defs"][name]
            return [(name, target)] if "properties" in target else structs(target)
        if node.get("type") == "array":
            return structs(node.get("items", {}))
        if node.get("type") == "object" and isinstance(node.get("additionalProperties"), dict):
            return structs(node["additionalProperties"])
        return [pair for member in node.get("anyOf", []) for pair in structs(member)]

    def walk(struct: dict, prefix: str, seen: tuple[str, ...]) -> None:
        for name, node, _ in fields(schema, struct):
            path = prefix + (camel(name) if index else name)
            kind = HOST_KINDS.get(node.get("x-monorm-type", ""))
            if kind is not None:
                found[path] = kind
            # An inline page has its own table: its paths are relative to that page.
            if node.get("x-monorm-type") == "inlines":
                continue
            for def_name, inner in structs(node):
                if def_name not in seen:
                    walk(inner, path + ".", (*seen, def_name))

    walk(schema["$defs"]["ResourceConfig"], "", ("ResourceConfig",))
    return sorted(found.items())


def schema_from_binary() -> dict:
    out = subprocess.run(["cargo", "run", "-q", "-p", "monorm-admin", "--bin", "admin-schema"], cwd=ROOT, check=True, capture_output=True, text=True)
    return json.loads(out.stdout)


def main() -> None:
    parser = argparse.ArgumentParser(description=(__doc__ or "").split("\n")[0])
    parser.add_argument("--schema", type=Path, help="a JSON Schema file; default: run the admin-schema binary")
    parser.add_argument("--python", type=Path, default=PYTHON_OUT)
    parser.add_argument("--typescript", type=Path, default=TYPESCRIPT_OUT)
    args = parser.parse_args()
    schema = json.loads(args.schema.read_text()) if args.schema else schema_from_binary()
    args.python.write_text(render_python(schema))
    args.typescript.write_text(render_typescript(schema))


if __name__ == "__main__":
    sys.exit(main())
