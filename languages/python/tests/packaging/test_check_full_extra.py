"""The `monorm[full]` extra must equal the first-party manifests minus the excluded list."""

import importlib.util
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "check_full_extra", Path(__file__).parents[2] / "packaging/check-full-extra.py"
)
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)


def tree(tmp_path, extra, manifests, version="0.1.0", manifest_version="0.1.0"):
    root = tmp_path
    members = ", ".join(f'"{name}"' for name in extra)
    pyproject = f'[project]\nname = "monorm"\nversion = "{version}"\n[project.optional-dependencies]\nfull = [{members}]\n'
    (root / "pyproject.toml").write_text(pyproject)
    for directory, ident in manifests.items():
        (root / "extensions" / directory).mkdir(parents=True)
        (root / "extensions" / directory / "monorm-extension.toml").write_text(f'id = "{ident}"\nversion = "{manifest_version}"\n')
    return root


MANIFESTS = {"timestamps": "monorm.timestamps", "soft-delete": "monorm.soft-delete", "file-storage": "monorm.file-storage"}


def test_matching_extra_passes_and_excluded_member_is_not_required(tmp_path):
    root = tree(tmp_path, ["monorm-native-tooling==0.1.0", "monorm-timestamps==0.1.0", "monorm-soft-delete==0.1.0"], MANIFESTS)
    assert checker.check(root) == []


def test_missing_member_fails(tmp_path):
    root = tree(tmp_path, ["monorm-native-tooling==0.1.0", "monorm-timestamps==0.1.0"], MANIFESTS)
    assert checker.check(root) == ["missing from monorm[full]: monorm-soft-delete==0.1.0"]


def test_excluded_or_unknown_member_fails(tmp_path):
    extra = ["monorm-native-tooling==0.1.0", "monorm-timestamps==0.1.0", "monorm-soft-delete==0.1.0", "monorm-file-storage==0.1.0"]
    root = tree(tmp_path, extra, MANIFESTS)
    assert checker.check(root) == ["not a first-party member of monorm[full]: monorm-file-storage==0.1.0"]


RC = {"version": "0.1.0rc1", "manifest_version": "0.1.0-rc.1"}


def test_a_release_candidate_tree_passes_with_pins_in_the_python_form(tmp_path):
    extra = ["monorm-native-tooling==0.1.0rc1", "monorm-timestamps==0.1.0rc1", "monorm-soft-delete==0.1.0rc1"]
    assert checker.check(tree(tmp_path, extra, MANIFESTS, **RC)) == []


def test_the_tooling_pin_follows_the_project_version(tmp_path):
    extra = ["monorm-native-tooling==0.1.0", "monorm-timestamps==0.1.0rc1", "monorm-soft-delete==0.1.0rc1"]
    assert checker.check(tree(tmp_path, extra, MANIFESTS, **RC)) == [
        "missing from monorm[full]: monorm-native-tooling==0.1.0rc1",
        "not a first-party member of monorm[full]: monorm-native-tooling==0.1.0",
    ]
