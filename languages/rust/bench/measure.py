#!/usr/bin/env python3
"""Measure generated consumers with warm dependencies; no live database is used."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[3]
TARGET = ROOT / "target" / "rust-client-measure"
ENV = dict(os.environ, CARGO_TARGET_DIR=str(TARGET))


def run(*args, cwd=ROOT):
    start = time.perf_counter()
    result = subprocess.run(args, cwd=cwd, env=ENV, capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError(result.stderr)
    return time.perf_counter() - start


def measure():
    run("cargo", "build", "-q", "-p", "monorm-cli")
    records = []
    with tempfile.TemporaryDirectory(prefix="monorm-rust-measure-") as temp:
        project = Path(temp)
        (project / "src").mkdir()
        (project / "Cargo.toml").write_text(f"""[package]
name="monorm-measured-consumer"
version="0.0.0"
edition="2021"
[workspace]
[dependencies]
monorm-client={{path={json.dumps(str(ROOT / "languages/rust"))}}}
tokio={{version="1",features=["rt-multi-thread","macros"]}}
""")
        (project / "src/main.rs").write_text("fn main() {}\n")
        run("cargo", "check", "--offline", "-q", cwd=project)
        run("cargo", "build", "--release", "--offline", "-q", cwd=project)
        for label, count, width, cyclic in [("wide", 1, 64, False), ("cyclic", 16, 16, True)]:
            models = []
            reads = []
            for i in range(count):
                fields = "\n".join(f" f{j} String?" for j in range(width))
                relations = ""
                if cyclic:
                    relations = f"\n next_id Int\n next M{(i + 1) % count} @relation(fields: [next_id], references: [id])\n previous M{(i - 1) % count}[]"
                models.append(f"model M{i} {{\n id Int @id\n{fields}{relations}\n}}")
                states = (
                    ["monorm_client::Unloaded"]
                    + ["monorm_client::Loaded" if j % 2 else "monorm_client::Unloaded" for j in range(width)]
                    + (["monorm_client::Unloaded"] if cyclic else [])
                )
                reads.append(
                    f"let rows=models::M{i}::objects.using(&db).all().await?; std::hint::black_box(rows);\nlet rows=models::M{i}::<{','.join(states)}>::query().using(&db).all().await?;std::hint::black_box(rows);"
                )
                if cyclic:
                    reads.append(
                        f"let rows=models::M{i}::objects.load(models::M{i}::next.load(models::M{(i + 1) % count}::f1)).using(&db).all().await?;std::hint::black_box(rows);"
                    )
            source = project / "schema.prisma"
            source.write_text("\n".join(models))
            generated = project / "src/models.rs"
            generation = run(
                str(TARGET / "debug/orm"),
                "--schema",
                str(source),
                "generate",
                "rust",
                "-o",
                str(generated),
                cwd=project,
            )
            (project / "src/main.rs").write_text(
                """#[allow(dead_code)]
mod models;
#[tokio::main]
async fn main()->monorm_client::Result<()> {
if let Ok(url)=std::env::var("MONORM_DATABASE_URL") {
let db=models::Database::connect(&url,1).await?;
"""
                + "\n".join(reads)
                + "\ndb.close().await;\n}\nOk(())\n}\n"
            )
            check = run("cargo", "check", "--offline", "-q", cwd=project)
            incremental = run("cargo", "check", "--offline", "-q", cwd=project)
            with (project / "src/main.rs").open("a") as file:
                file.write("\n// Incremental consumer edit.\n")
            incremental_edit = run("cargo", "check", "--offline", "-q", cwd=project)
            release = run("cargo", "build", "--release", "--offline", "-q", cwd=project)
            records.append(
                dict(
                    schema=label,
                    models=count,
                    scalar_fields_per_model=width + 1 + int(cyclic),
                    cyclic=cyclic,
                    shapes_per_model=3 if cyclic else 2,
                    generated_bytes=generated.stat().st_size,
                    generation_seconds=generation,
                    check_seconds=check,
                    no_change_check_seconds=incremental,
                    edited_check_seconds=incremental_edit,
                    release_seconds=release,
                    binary_bytes=(TARGET / "release/monorm-measured-consumer").stat().st_size,
                )
            )
    (ROOT / "languages/rust/bench/selection-results.json").write_text(
        json.dumps(
            dict(rustc=subprocess.check_output(["rustc", "--version"], text=True).strip(), records=records), indent=2
        )
        + "\n"
    )


if __name__ == "__main__":
    measure()
