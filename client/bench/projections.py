#!/usr/bin/env python3
"""Compare projection type checking with warm dependencies on the current machine."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
ENV = dict(os.environ, CARGO_TARGET_DIR=str(ROOT / "target/consumer-checks"), CARGO_INCREMENTAL="0")


def check(project):
    start = time.perf_counter()
    result = subprocess.run(
        ["cargo", "check", "--offline", "--quiet", "--manifest-path", str(project / "Cargo.toml")],
        env=ENV, capture_output=True, text=True,
    )
    if result.returncode:
        raise RuntimeError(result.stderr)
    return time.perf_counter() - start


def main():
    # Recreate build-script output after cargo clean, without changing timed checks.
    subprocess.run(
        ["cargo", "build", "--quiet", "-p", "orm-client"], cwd=ROOT,
        env=dict(os.environ, CARGO_TARGET_DIR=str(ROOT / "target"), CARGO_INCREMENTAL="0"),
        check=True, capture_output=True, text=True,
    )
    generated = max((ROOT / "target/debug/build").glob("orm-client-*/out/models.rs"), key=lambda p: p.stat().st_mtime)
    variants = {
        "model_baseline": "let rows=User::objects.require().using(db).all().await?;std::hint::black_box(rows);",
        "tuple": "let rows=User::objects.select((User::email,User::nickname,User::TYPE.upper())).require().using(db).all().await?;std::hint::black_box(rows);",
        "local_named": "let rows=orm_client::select_named!(User::objects;email=>User::email,nickname=>User::nickname,kind=>User::TYPE.upper()).require().using(db).all().await?;std::hint::black_box(rows);",
        "exported_model_shape": "let rows=orm_client::select_named!(User::objects=>Contact;person=>orm_client::model::<User>(),nickname=>User::nickname).require().using(db).all().await?;for row in rows {let _: &String=email(&row.person);}",
    }
    records = []
    with tempfile.TemporaryDirectory(prefix="orm-projection-cost-") as temp:
        project = Path(temp)
        (project / "src").mkdir()
        (project / "Cargo.toml").write_text(f'''[package]
name="orm-projection-cost"
version="0.0.0"
edition="2021"
[workspace]
[dependencies]
orm-client={{path={json.dumps(str(ROOT / 'client'))}}}
''')
        (project / "src/models.rs").write_text(generated.read_text())
        prefix = '''#![allow(dead_code)]
mod models;
use models::User;
orm_client::named_result! {pub struct Contact {person,nickname}}
fn email<S:models::UserShape<Email=String>>(row:&S)->&String {row.email()}
'''
        # Warm all dependencies and this generated schema before comparing variants.
        (project / "src/lib.rs").write_text(prefix)
        check(project)
        for label, body in variants.items():
            source = prefix + "pub async fn consume(db:&models::Database)->orm_client::Result<()> {" + body + "Ok(())}\n"
            samples = []
            for repeat in range(3):
                (project / "src/lib.rs").write_text(source + f"// rebuild {repeat}\n")
                samples.append(check(project))
            records.append({"variant": label, "consumer_source_bytes": len(source.encode()), "warm_dependency_check_seconds": samples})
    result = {
        "measurement": "measured on a loaded machine; warm dependencies; incremental compilation disabled",
        "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
        "generated_model_bytes": generated.stat().st_size,
        "records": records,
        "limitations": "Includes generated model type checking and cargo overhead; does not isolate macro expansion, runtime, release monomorphization or binary size. Three local observations per variant, not acceptance thresholds.",
    }
    output = ROOT / "client/bench/projection-results.json"
    output.write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    main()
