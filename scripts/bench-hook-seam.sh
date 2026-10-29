#!/usr/bin/env bash
# Gate: the hook seam adds no measured cost to a model with no hook.
# Usage: scripts/bench-hook-seam.sh BASELINE_CHECKOUT [ROUNDS=15]
# BASELINE_CHECKOUT is a checkout of `main` (for example `git worktree add ../monorm-main main`).
# Run it on a quiet machine. It builds both checkouts into separate venvs under a temp directory,
# runs them interleaved twice, and fails when the median of this checkout is slower than the baseline by more than the noise.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
BASE=$(cd "${1:?usage: bench-hook-seam.sh BASELINE_CHECKOUT [ROUNDS]}" && pwd)
ROUNDS=${2:-15}
PYTHON=${PYTHON:-python3}
WORK=$(mktemp -d "${TMPDIR:-/tmp}/bench-hook-seam-XXXXXX")
build() { # source dir, venv dir
  uv venv --python "$PYTHON" "$2" >/dev/null
  uv pip install --python "$2/bin/python" maturin >/dev/null
  (cd "$1" && . "$2/bin/activate" && python languages/python/packaging/stage.py >/dev/null && maturin develop --release >/dev/null)
}
build "$BASE" "$WORK/venv-base"
build "$ROOT" "$WORK/venv-head"
for pass in 1 2; do
  "$WORK/venv-base/bin/python" "$ROOT/scripts/bench-hook-seam.py" base "$ROUNDS" "$WORK/base-$pass.json"
  "$WORK/venv-head/bin/python" "$ROOT/scripts/bench-hook-seam.py" head "$ROUNDS" "$WORK/head-$pass.json"
done
"$WORK/venv-head/bin/python" - "$WORK" <<'PY'
import json, statistics, sys
work, failed = sys.argv[1], False
for key in ("insert_many_10000", "read_1000"):
    runs = {w: [t for p in (1, 2) for t in json.load(open(f"{work}/{w}-{p}.json"))[key]] for w in ("base", "head")}
    base, head = statistics.median(runs["base"]), statistics.median(runs["head"])
    q = statistics.quantiles(runs["base"], n=4)
    noise = max(q[2] - q[0], 0.02 * base)
    slower = head - base
    verdict = "FAIL" if slower > noise else "ok"
    failed |= verdict == "FAIL"
    print(f"{key}: base {base*1000:.1f} ms, head {head*1000:.1f} ms, diff {slower*1000:+.1f} ms, noise (IQR of base, at least 2%) {noise*1000:.1f} ms: {verdict}")
sys.exit(1 if failed else 0)
PY
