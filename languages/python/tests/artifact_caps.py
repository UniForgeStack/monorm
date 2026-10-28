"""The native artifact's capabilities and the installed extension ids."""

import json

from monorm import _native
from monorm._packages import resolve_extensions


def artifact_capabilities() -> list[str]:
    return json.loads(_native.native_artifact())["capabilities"]


def has_extension(*ids: str) -> bool:
    """True when an installed package contains every named extension, such as `monorm.file-storage`.

    A missing `monorm.timestamps`, `monorm.soft-delete`, `monorm.locking`, `monorm.inherit`, `monorm.query-defaults`, `monorm.generic`, `monorm.proxy`, `monorm.composition` or `monorm.file-storage` package fails with an assert.
    """
    extensions = resolve_extensions()
    for ident in ids:
        if ident in {"monorm.timestamps", "monorm.soft-delete", "monorm.locking", "monorm.inherit", "monorm.query-defaults", "monorm.generic", "monorm.proxy", "monorm.composition", "monorm.file-storage"}:
            assert ident in extensions, f"install the {ident} package with scripts/install-extension-packages.sh"
    return all(i in extensions for i in ids)
