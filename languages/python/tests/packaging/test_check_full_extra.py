"""The `monorm[full]` extra must equal the first-party manifests minus the excluded list."""

import importlib.util
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "check_full_extra", Path(__file__).parents[2] / "packaging/check-full-extra.py"
)
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)


def tree(tmp_path, extra, manifests):
    root = tmp_path
    members = ", ".join(f'"{name}"' for name in extra)
    (root / "pyproject.toml").write_text(f'[project]\nname = "orm"\n[project.optional-dependencies]\nfull = [{members}]\n')
    for directory, ident in manifests.items():
        (root / "extensions" / directory).mkdir(parents=True)
        (root / "extensions" / directory / "monorm-extension.toml").write_text(f'id = "{ident}"\nversion = "0.1.0"\n')
    return root


MANIFESTS = {"timestamps": "monorm.timestamps", "soft-delete": "monorm.soft-delete", "file-storage": "monorm.file-storage"}


def test_matching_extra_passes_and_excluded_member_is_not_required(tmp_path):
    root = tree(tmp_path, [checker.NATIVE, "monorm-timestamps==0.1.0", "monorm-soft-delete==0.1.0"], MANIFESTS)
    assert checker.check(root) == []


def test_missing_member_fails(tmp_path):
    root = tree(tmp_path, [checker.NATIVE, "monorm-timestamps==0.1.0"], MANIFESTS)
    assert checker.check(root) == ["missing from monorm[full]: monorm-soft-delete==0.1.0"]


def test_excluded_or_unknown_member_fails(tmp_path):
    extra = [checker.NATIVE, "monorm-timestamps==0.1.0", "monorm-soft-delete==0.1.0", "monorm-file-storage==0.1.0"]
    root = tree(tmp_path, extra, MANIFESTS)
    assert checker.check(root) == ["not a first-party member of monorm[full]: monorm-file-storage==0.1.0"]
