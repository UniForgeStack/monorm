"""Loader validation in isolation: avoid importing orm before selecting a profile."""

import importlib.util
import json
from pathlib import Path
from types import ModuleType
from unittest.mock import patch

import pytest

spec = importlib.util.spec_from_file_location("monorm_profiles", Path(__file__).parents[2] / "monorm/_profiles.py")
profiles = importlib.util.module_from_spec(spec)
spec.loader.exec_module(profiles)


BUILD = {"features": ["sqlite"], "rustc": "rustc 1.0.0", "target": "t", "profile": "release", "revision": "r"}


def artifact(name, **overrides):
    tooling = name == "tooling"
    metadata = {
        "abi": 1,
        "version": "0.1.0",
        "language": "python",
        "profile": name,
        "backends": list(profiles.PROFILES[name]),
        "adapters": [],
        "build": BUILD,
        "capabilities": {
            "cli": tooling,
            "generate-python": tooling,
            "generate-typescript": tooling,
        },
    }
    metadata.update(overrides)
    module = ModuleType("fake_engine")
    module.profile_metadata = lambda: json.dumps(metadata)
    return module


@pytest.mark.parametrize(
    "installed,selector,message",
    [
        ([], None, "installed: none"),
        (["postgres", "sqlite"], None, "extras do not combine"),
        (["sqlite"], "postgres", "is not installed"),
        (["sqlite"], "unknown", "unknown MONORM_PROFILE"),
    ],
)
def test_bad_selection_fails_without_loading_engine(installed, selector, message):
    environment = {} if selector is None else {"MONORM_PROFILE": selector}
    with (
        patch.dict("os.environ", environment, clear=True),
        patch.object(
            profiles.importlib.util,
            "find_spec",
            side_effect=lambda name: object() if name.removeprefix("monorm_native_") in installed else None,
        ),
        patch.object(profiles.importlib, "import_module") as load,
    ):
        with pytest.raises(ImportError, match=message):
            profiles.load()
        load.assert_not_called()


def test_selector_loads_exactly_one_profile():
    module = artifact("combined")
    with (
        patch.dict("os.environ", {"MONORM_PROFILE": "combined"}, clear=True),
        patch.object(profiles.importlib.util, "find_spec", return_value=object()),
        patch.object(profiles.importlib, "import_module", return_value=module) as load,
    ):
        assert profiles.load() is module
        load.assert_called_once_with("monorm_native_combined._native")


@pytest.mark.parametrize(
    "override",
    [
        {"abi": 2},
        {"version": "0.2.0"},
        {"language": "node"},
        {"build": None},
        {"build": {**BUILD, "features": "sqlite"}},
        {"build": {**BUILD, "revision": None}},
        {"backends": ["postgres", "sqlite"]},
        {"capabilities": {"cli": True}},
        {"adapters": ["reference-loading"]},
    ],
)
def test_incompatible_metadata(override):
    with pytest.raises(ImportError, match="incompatible"):
        profiles.validate(artifact("sqlite", **override), "sqlite")


def test_tooling_profile_is_not_another_profile():
    profiles.validate(artifact("tooling"), "tooling")
    with pytest.raises(ImportError, match="incompatible"):
        profiles.validate(artifact("tooling"), "combined")
    assert "full" not in profiles.PROFILES


def test_installed_artifact_embeds_its_build_record():
    import monorm  # noqa: F401  (selects and validates the installed profile)
    from monorm import _native

    metadata = json.loads(_native.profile_metadata())
    build = metadata["build"]
    assert set(metadata["backends"]) <= set(build["features"])
    assert build["rustc"].startswith("rustc ") and build["target"] and build["revision"]


def test_installed_artifact_reports_no_adapters():
    from monorm import _capabilities, _native

    assert json.loads(_native.profile_metadata())["adapters"] == []
    assert _capabilities.ADAPTERS == frozenset()
