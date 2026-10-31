#!/usr/bin/env python3
"""Print the check groups that a change can break.

    affected.py [--merge-base] [--format json|lines] <base> <head>
    affected.py --all [--format json|lines]

The groups are the jobs of .github/workflows/checks.yml and the steps of the local main check.
A path that no rule matches selects every group, and so does a base that is missing, not an ancestor of head
(a forced push) or not known to git (a new branch, a shallow clone).
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# `names` always runs: it is cheap and the rename and metadata checks read the whole tree.
ALWAYS = {"names"}
BASE_GROUPS = {
    "rust",
    "python",
    "node",
    "create-monorm",
    "packages",
    "release-scripts",
    "names",
    "docs",
    "test-twins",
}

EVERY = set(BASE_GROUPS)  # a rule that selects every group

# The first rule that matches a path wins. A rule is (regex over the repo-relative path, groups).
# The groups come from what each test reads: crates/core, crates/engine and crates/contracts sit under
# every binding and every extension, the wheel and the npm package hold all extensions, and
# pytest collects languages/python/tests and scripts/tests.
RULES: list[tuple[str, set[str]]] = [
    # Anything that changes how CI itself works runs everything.
    (r"^\.github/(workflows|actions)/", EVERY),
    (r"^scripts/ci/", EVERY),
    (r"^(crates/(core|engine|contracts)/|Cargo\.(lock|toml)$|pyproject\.toml$|uv\.lock$)", EVERY),
    # `documented_schemas_parse` (crates/core) compiles the `prisma` blocks of these two files.
    (r"^docs/schema/", {"docs", "rust"}),
    # Files that no test reads.
    (r"^CHANGELOG\.md$", {"docs", "release-scripts"}),
    (r"^scripts/tests/rename/", {"names", "python"}),
    (r"^(docs/|LICENSE$|.*\.md$)", {"docs"}),
    # Rust crates and the bindings.
    (r"^crates/cli/", {"rust", "python", "node", "create-monorm", "packages"}),
    # `npm run assemble` builds and runs monorm-extension-package for each bundled extension.
    (r"^crates/extension-package/", {"rust", "packages", "python", "node", "create-monorm"}),
    (r"^languages/rust/", {"rust"}),
    (r"^languages/python/native/", {"rust", "python", "packages"}),
    # The rust job runs stage.py, and the cargo typing tests type-check generated code against the `monorm` package.
    (r"^languages/python/packaging/", {"rust", "python", "packages"}),
    (r"^languages/python/tests/", {"python", "test-twins"}),
    (r"^languages/python/", {"rust", "python", "packages"}),
    (r"^languages/js/native/", {"rust", "node", "create-monorm", "packages"}),
    (r"^languages/js/create-monorm/", {"create-monorm"}),
    (r"^languages/js/test/", {"node", "test-twins"}),
    (r"^languages/js/packaging/", {"node", "packages"}),
    (r"^languages/js/auth/", {"node", "packages"}),
    # The cargo typing tests compile generated code with `tsc` against these files and node_modules/@types.
    (r"^languages/js/package(-lock)?\.json$", {"rust", "node", "create-monorm", "packages", "release-scripts"}),
    (r"^languages/js/src/(index|ext)\.ts$", {"rust", "node", "create-monorm", "packages"}),
    (r"^languages/js/", {"node", "create-monorm", "packages"}),
    (r"^apps/auth/", {"python", "node", "packages"}),
    (r"^extensions/[^/]+/", {"rust", "python", "node", "packages"}),
    (r"^(examples|fixtures)/", {"rust", "python", "node"}),
    (r"^tests/outside-extension/", {"rust", "packages"}),
    (r"^bench/internal/", set()),  # its smoke test is opt-in (MONORM_BENCH_SMOKE), so no CI job runs it
    (r"^bench/", {"python"}),
    # Scripts and their tests.
    (r"^scripts/(release/|check-glibc\.py$|tests/test_(check_glibc|release_versions|package_metadata)\.py$)", {"release-scripts", "python"}),
    (r"^scripts/rename/", {"names", "python"}),
    (r"^scripts/(check-extension-names\.py|tests/test_check_extension_names\.py)$", {"packages", "python"}),
    (
        r"^scripts/(check-extension-packages\.sh|check-assembled-[a-z-]+\.py|verify-reference-profile\.py"
        r"|tests/(test_extension_package\.py|builtin_copy\.py|[a-z]+_consumer\.[a-z]+))$",
        {"packages", "python"},
    ),
    (r"^scripts/(check-thin-bindings\.py|tests/test_check_thin_bindings\.py)$", {"python"}),  # `names` runs the check itself
    (r"^scripts/check-init\.sh$", set()),  # no CI job runs it
    (r"^scripts/(tests/test_internal_bench\.py|bench-hook-seam\.(py|sh))$", {"python"}),
    (r"^(scripts/check-test-twins\.py|scripts/tests/test_check_test_twins\.py|languages/test-twins\.toml)$", {"test-twins", "python"}),
]
COMPILED = [(re.compile(pattern), groups) for pattern, groups in RULES]
# The paths that scripts/release/versions.py reads (TEXT_SITES and JSON_SITES); a change there also runs its check.
VERSION_SITES = re.compile(
    r"^(crates/cli/Cargo\.toml|languages/(python|js)/native/Cargo\.toml|extensions/[^/]+/monorm-extension\.toml"
    r"|languages/js/create-monorm/package\.json|languages/js/src/native\.ts|tests/outside-extension/Cargo\.lock)$"
)


def groups_for(path: str) -> set[str] | None:
    """The groups of one path, or None when no rule matches."""
    for pattern, groups in COMPILED:
        if pattern.match(path):
            return groups | {"release-scripts"} if VERSION_SITES.match(path) else set(groups)
    return None


def plan(files: list[str]) -> tuple[set[str], str]:
    """The groups for a list of changed paths, and the reason when it is every group."""
    selected = set(ALWAYS)
    for path in files:
        groups = groups_for(path)
        if groups is None:
            return set(BASE_GROUPS), f"no rule matches {path}"
        if groups >= BASE_GROUPS:
            return set(BASE_GROUPS), f"{path} selects every group"
        selected |= groups
    return selected, ""


def git(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(["git", *args], cwd=ROOT, capture_output=True, text=True)


def changed_files(base: str, head: str, merge_base: bool) -> list[str] | str:
    """The changed paths, or the reason why the diff is unknown."""
    if not base or set(base) == {"0"}:
        return "no base commit (a new branch)"
    if git("cat-file", "-e", f"{base}^{{commit}}").returncode != 0:
        return f"base {base} is not in the clone"
    if not merge_base and git("merge-base", "--is-ancestor", base, head).returncode != 0:
        return f"base {base} is not an ancestor of {head} (a forced push)"
    diff = git("diff", "--name-only", "--no-renames", f"{base}...{head}" if merge_base else f"{base}..{head}")
    if diff.returncode != 0:
        return f"git diff failed: {diff.stderr.strip()}"
    return [line for line in diff.stdout.splitlines() if line]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("base", nargs="?", default="")
    parser.add_argument("head", nargs="?", default="HEAD")
    parser.add_argument("--all", action="store_true", help="select every group (schedule, dispatch, tag)")
    parser.add_argument("--merge-base", action="store_true", help="diff from the merge base (pull request)")
    parser.add_argument("--format", choices=["json", "lines"], default="json")
    args = parser.parse_args()

    files = "--all given" if args.all else changed_files(args.base, args.head, args.merge_base)
    groups, reason = (set(BASE_GROUPS), files) if isinstance(files, str) else plan(files)
    result = sorted(groups)
    if args.format == "lines":
        print("\n".join(result))
    else:
        print(json.dumps({"all": bool(reason), "reason": reason, "groups": result}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
