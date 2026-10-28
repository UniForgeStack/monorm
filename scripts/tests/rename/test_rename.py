"""Tests of scripts/rename/rename.py on a small fixture tree (no database)."""

from __future__ import annotations

import importlib.util
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
FIXTURES = HERE / "fixtures"
TOOL = HERE.parents[1] / "rename" / "rename.py"

spec = importlib.util.spec_from_file_location("rename_tool", TOOL)
assert spec and spec.loader
rename = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = rename
spec.loader.exec_module(rename)

CHANGED = {
    "Cargo.toml": '[package]\nname = "monorm-core"\n\n[dependencies]\nmonorm-engine = { path = "crates/engine" }\nserde = "1"\n',
    "crates/engine/src/lib.rs": (
        "//! Kept: ORM_TEST_DATABASE_URL names the test database `orm_test`.\n"
        "use monorm_core::ir;\n\n"
        "pub fn url() -> Option<String> {\n"
        '    std::env::var("MONORM_DATABASE_URL").ok()\n}\n'
    ),
    "pyproject.toml": '[project]\nname = "monorm"\ndependencies = ["monorm-native-postgres"]\n\n[tool.monorm]\napps = []\n',
    "languages/python/monorm/__init__.py": '"""Fixture package."""\nfrom monorm.base import Model\n\n__all__ = ["Model"]\n',
    "languages/python/tests/test_model.py": "import monorm\nfrom monorm import Model\nimport monorm_core\n\n\ndef test_model():\n    assert orm.Model is Model\n",
    "languages/js/package.json": '{\n  "name": "@ufstack/monorm",\n  "dependencies": {\n    "@ufstack/monorm-inherit": "^0.1.0"\n  }\n}\n',
    "languages/js/src/index.ts": (
        'import { ext } from "@ufstack/monorm/ext";\n'
        'import inherit from "@ufstack/monorm-inherit";\n'
        'import kept from "@ufstack/monorm-inherit";\n\n'
        "export { ext, inherit, kept };\n"
    ),
    "languages/js/src/cli.ts": "const url = process.env.MONORM_DATABASE_URL ?? process.env.ORM_TEST_DATABASE_URL;\nconst profile = process.env.MONORM_PROFILE;\nexport { url, profile };\n",
    "languages/python/monorm/base.py": "class Model:\n    pass\n",
    "extensions/inherit/monorm-extension.toml": 'id = "inherit"\nname = "monorm-inherit"\npython = "monorm_inherit"\n',
    "extensions/inherit/python/monorm_inherit/__init__.py": "from monorm import Model\n\n\nclass Inherit(Model):\n    pass\n",
    "apps/auth/python/monorm_auth/__init__.py": '"""The auth app."""\nimport monorm_core\n',
    ".github/workflows/ci.yml": (
        "jobs:\n  test:\n    env:\n      ORM_TEST_DATABASE_URL: postgres://postgres:postgres@localhost/orm_test\n"
        "    steps:\n      - run: python -m monorm generate\n      - run: monorm generate typescript\n"
    ),
    "scripts/check.sh": "#!/bin/sh\nMONORM_PROFILE=postgres python -m monorm check\nnpx @ufstack/monorm generate\n",
    "README.md": "# Fixture\n\nAn ORM maps rows to objects. Most ORMs do this.\n\n```\npip install monorm\n```\n",
    "docs/guide.md": "Run `monorm generate typescript` after a change.\nThe ORM docs say the same.\n",
    "docs/notes.md": "The package monorm_core is old text.\n",
    "Cargo.lock": '[[package]]\nname = "monorm-core"\nversion = "0.1.0"\n\n[[package]]\nname = "monorm-engine"\nversion = "0.1.0"\n',
    "crates/engine/Cargo.toml": (
        '[package]\nname = "monorm-engine"\n\n[dependencies]\n'
        'monorm_core = { package = "monorm-core", path = "../.." }\n'
    ),
}
MOVED_AWAY = {
    "languages/python/orm/__init__.py",
    "languages/python/orm/base.py",
    "extensions/inherit/orm-extension.toml",
    "extensions/inherit/python/orm_inherit/__init__.py",
    "apps/auth/python/orm_auth/__init__.py",
}
UNCHANGED = {
    "bench/compare/run.py",
    "bench/compare/results/2026/run.json",
    "scripts/rename/notes.txt",
    "assets/blob.bin",
}


def sh(root: Path, *args: str) -> str:
    return subprocess.run(args, cwd=root, check=True, capture_output=True, text=True).stdout


@pytest.fixture
def repo(tmp_path: Path) -> Path:
    root = tmp_path / "repo"
    shutil.copytree(FIXTURES / "tree", root)
    sh(root, "git", "init", "-q")
    sh(root, "git", "add", "-A")
    sh(root, "git", "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "fixture")
    return root


def run(root: Path, mode: str, rules: Path | None = None, allow: Path | None = None) -> tuple[int, list[str]]:
    lines: list[str] = []
    code = rename.run(
        root,
        mode,
        rules or FIXTURES / "rules.toml",
        allow or FIXTURES / "allow.toml",
        out=lines.append,
    )
    return code, lines


def snapshot(root: Path) -> dict[str, bytes]:
    return {
        str(p.relative_to(root)): p.read_bytes()
        for p in sorted(root.rglob("*"))
        if p.is_file() and ".git" not in p.relative_to(root).parts
    }


def test_apply_builds_the_expected_tree(repo: Path) -> None:
    before = snapshot(repo)
    code, lines = run(repo, "apply")
    assert code == 0, lines
    after = snapshot(repo)
    for rel, text in CHANGED.items():
        assert after[rel].decode() == text, rel
    for rel in MOVED_AWAY:
        assert rel not in after
    for rel in UNCHANGED:
        assert after[rel] == before[rel], rel
    assert sorted(after) == sorted(set(before) - MOVED_AWAY | {p for p in CHANGED if p not in before})


def test_second_apply_changes_nothing(repo: Path) -> None:
    assert run(repo, "apply")[0] == 0
    sh(repo, "git", "add", "-A")
    once = snapshot(repo)
    code, lines = run(repo, "apply")
    assert code == 0, lines
    assert snapshot(repo) == once
    assert "applied: 0 moves, 0 files rewritten" in lines


def test_new_names_do_not_match_again(repo: Path) -> None:
    assert run(repo, "apply")[0] == 0
    rules, _ = rename.load_rules(FIXTURES / "rules.toml")
    for path in ("languages/js/src/index.ts", "Cargo.toml", "pyproject.toml"):
        text = (repo / path).read_text()
        assert rename.rewrite(text, path, rules) == text


def test_uppercase_prose_stays(repo: Path) -> None:
    assert run(repo, "apply")[0] == 0
    text = (repo / "README.md").read_text()
    assert "An ORM maps rows" in text and "Most ORMs" in text
    assert "pip install monorm" in text
    assert run(repo, "check")[0] == 0


def test_protected_names_stay(repo: Path) -> None:
    assert run(repo, "apply")[0] == 0
    ci = (repo / ".github/workflows/ci.yml").read_text()
    assert "ORM_TEST_DATABASE_URL" in ci and "localhost/orm_test" in ci
    assert (repo / "bench/compare/run.py").read_text().count("ORM_BENCH_ITERS") == 1
    assert '"orm-all"' in (repo / "bench/compare/run.py").read_text()


def test_range_miss_stops_before_any_write(repo: Path, tmp_path: Path) -> None:
    rules = tmp_path / "narrow.toml"
    rules.write_text(
        (FIXTURES / "rules.toml")
        .read_text()
        .replace('expect = [2, 2]\n\n[[rule]]\nid = "js-import', 'expect = [9, 9]\n\n[[rule]]\nid = "js-import', 1)
    )
    before = snapshot(repo)
    code, lines = run(repo, "apply", rules=rules)
    assert code == 1
    assert any(line.startswith("error: rule npm-scope: 2 matches, expected 9..9") for line in lines)
    assert "stopped before any write" in lines
    assert snapshot(repo) == before
    assert sh(repo, "git", "status", "--porcelain") == ""


def test_dry_run_reports_and_writes_nothing(repo: Path) -> None:
    before = snapshot(repo)
    code, lines = run(repo, "dry-run")
    assert code == 0
    assert any(line.startswith("  npm-scope: 2 in 2 files") for line in lines)
    assert "uncovered hits: 0" in lines
    assert snapshot(repo) == before


def test_dry_run_fails_on_an_uncovered_hit(repo: Path) -> None:
    (repo / "crates/engine/src/extra.rs").write_text("use ORM_NEW_FLAG;\n")
    sh(repo, "git", "add", "-A")
    code, lines = run(repo, "dry-run")
    assert code == 1
    assert "uncovered hits: 1" in lines


def test_env_rule_outside_its_range_stops_the_apply(repo: Path, tmp_path: Path) -> None:
    rules = tmp_path / "env.toml"
    rules.write_text(
        (FIXTURES / "rules.toml")
        .read_text()
        .replace("expect = [2, 2]\n\n[[env_rename]]", "expect = [9, 9]\n\n[[env_rename]]", 1)
    )
    code, lines = run(repo, "apply", rules=rules)
    assert code == 1
    assert any(line.startswith("error: rule env-ORM_DATABASE_URL: 2 matches, expected 9..9") for line in lines)


def test_allow_entry_does_not_hide_another_name_on_the_line(repo: Path) -> None:
    assert run(repo, "apply")[0] == 0
    (repo / "crates/engine/src/extra.rs").write_text("let a = (ORM_TEST_DATABASE_URL, orm_core);\n")
    sh(repo, "git", "add", "-A")
    code, lines = run(repo, "check")
    assert code == 1
    assert any(line.startswith("crates/engine/src/extra.rs:1:") for line in lines)
    (repo / "crates/engine/src/extra.rs").write_text("let a = ORM_TEST_DATABASE_URL;\n")
    assert run(repo, "check")[0] == 0


def test_guard_pattern_exclude(repo: Path, tmp_path: Path) -> None:
    allow = tmp_path / "allow.toml"
    allow.write_text(
        (FIXTURES / "allow.toml").read_text()
        + '\n[[guard]]\nid = "bare"\nfiles = ["**"]\nexclude = ["**/*.md"]\npattern = "orm package"\n'
    )
    assert run(repo, "apply")[0] == 0
    (repo / "docs/prose.md").write_text("The orm package.\n")
    sh(repo, "git", "add", "-A")
    assert run(repo, "check", allow=allow)[0] == 0
    (repo / "crates/engine/src/extra.rs").write_text("// the orm package\n")
    sh(repo, "git", "add", "-A")
    code, lines = run(repo, "check", allow=allow)
    assert code == 1 and len(lines) == 1


def test_move_onto_existing_target_is_refused(repo: Path) -> None:
    (repo / "extensions/inherit/python/monorm_inherit").mkdir()
    (repo / "extensions/inherit/python/monorm_inherit/__init__.py").write_text("")
    sh(repo, "git", "add", "-A")
    before = snapshot(repo)
    with pytest.raises(rename.RenameError, match="move target exists"):
        run(repo, "apply")
    assert snapshot(repo) == before


def test_check_passes_on_a_clean_tree(repo: Path) -> None:
    assert run(repo, "apply")[0] == 0
    sh(repo, "git", "add", "-A")
    code, lines = run(repo, "check")
    assert (code, lines) == (0, [])


def test_check_fails_on_a_planted_old_name(repo: Path) -> None:
    assert run(repo, "apply")[0] == 0
    (repo / "crates/engine/src/extra.rs").write_text("use orm_core::ir;\n")
    sh(repo, "git", "add", "-A")
    code, lines = run(repo, "check")
    assert code == 1
    assert "crates/engine/src/extra.rs:1: use orm_core::ir;" in lines


def test_check_fails_on_a_stale_allow_entry(repo: Path, tmp_path: Path) -> None:
    assert run(repo, "apply")[0] == 0
    allow = tmp_path / "allow.toml"
    allow.write_text(
        (FIXTURES / "allow.toml").read_text()
        + '\n[[allow]]\nglob = "**"\npattern = "never_present_anywhere"\nreason = "stale on purpose"\n'
    )
    code, lines = run(repo, "check", allow=allow)
    assert code == 1
    assert any(line.startswith("stale allow entry:") and "never_present_anywhere" in line for line in lines)


def test_allow_entry_needs_a_reason(repo: Path, tmp_path: Path) -> None:
    allow = tmp_path / "allow.toml"
    allow.write_text('[[guard]]\nid = "x"\npattern = "orm_"\n\n[[allow]]\nglob = "**"\npattern = "x"\nreason = " "\n')
    with pytest.raises(rename.RenameError, match="without a reason"):
        run(repo, "check", allow=allow)


def test_glob_translation() -> None:
    assert rename.matches_any("a/b/c.toml", ["**/*.toml"])
    assert rename.matches_any("c.toml", ["**/*.toml"])
    assert not rename.matches_any("a/b/c.toml", ["*.toml"])
    assert rename.matches_any("bench/x/results/y.json", ["bench/**/results/**"])


def test_tool_and_test_files_are_out_of_scope() -> None:
    assert not rename.is_scoped("scripts/rename/rules.toml")
    assert not rename.is_scoped("scripts/tests/rename/fixtures/tree/Cargo.toml")
    assert rename.is_scoped("scripts/tests/test_extension_package.py")


def test_move_onto_an_untracked_target_is_refused(repo: Path) -> None:
    (repo / "languages/python/monorm").mkdir()
    (repo / "languages/python/monorm/stray.txt").write_text("x\n")
    before = snapshot(repo)
    with pytest.raises(rename.RenameError, match="move target exists"):
        run(repo, "apply")
    assert snapshot(repo) == before
