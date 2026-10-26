"""bench/compare/report.py shows speed-ups as "N× faster" with a Summary block."""

import re
import subprocess
import sys
from pathlib import Path

COMPARE = Path(__file__).resolve().parents[3] / "bench" / "compare"
RUN = COMPARE / "results" / "2026-10-21-c9bbf7c"


def geomean_of(text: str, line_start: str) -> float:
    line = next(x for x in text.splitlines() if x.startswith(line_start))
    return float(re.search(r"geometric mean (\d+\.\d+)×", line).group(1))


def test_report_summary_speedups(tmp_path):
    out = tmp_path / "summary.md"
    subprocess.run([sys.executable, str(COMPARE / "report.py"), str(RUN), str(out)], check=True)
    text = out.read_text()
    summary = text.split("## Summary")[1].split("## Python")[0]
    assert abs(geomean_of(summary, "- Python vs `django-async`") - 1.95) <= 0.05
    assert abs(geomean_of(summary, "- Node vs `prisma`") - 1.77) <= 0.05
    assert "end to end" in summary and "approximate" in summary
    assert "%" not in text
    assert "(slower)" in text
