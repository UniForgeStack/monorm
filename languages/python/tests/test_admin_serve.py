"""`python -m monorm admin`: the server of a project on SQLite, started as a process of its own."""

import json
import os
import signal
import subprocess
import sys
import urllib.request

import pytest

from monorm.__main__ import main as cli

SECRET = "s" * 40
SCHEMA = 'datasource db {\n  provider = "sqlite"\n}\n\nmodel Book {\n  id    BigInt @id @default(autoincrement())\n  title String\n\n  @@map("books")\n}\n'


def project(tmp_path, monkeypatch, migrate=True):
    (tmp_path / "schema.prisma").write_text(SCHEMA)
    monkeypatch.chdir(tmp_path)
    assert cli(["admin", "init"]) == 0
    assert cli(["generate"]) == 0
    assert cli(["makemigrations"]) == 0
    url = f"sqlite://{tmp_path}/admin.db"
    if migrate:
        assert cli(["migrate", "--url", url]) == 0
    return url


def start(tmp_path, url, **env):
    environ = {**os.environ, "MONORM_ADMIN_PORT": "0", "MONORM_DATABASE_URL": url or "", "MONORM_ADMIN_SECRET": SECRET, **env}
    return subprocess.Popen([sys.executable, "-m", "monorm", "admin"], cwd=tmp_path, env=environ, stderr=subprocess.PIPE, text=True)


def listening(proc):
    """The URL from the `listening` log line."""
    for line in proc.stderr:
        entry = json.loads(line)
        if entry["msg"] == "listening":
            return entry["url"]
    raise AssertionError("the server ended without a listening line")


def get(url):
    try:
        with urllib.request.urlopen(url, timeout=10) as response:
            return response.status, dict(response.headers), response.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, dict(e.headers), e.read().decode()


def run_to_end(proc):
    err = proc.stderr.read()
    return proc.wait(timeout=30), err


def test_monorm_admin_serves_health_with_a_request_id_and_stops_on_sigterm(tmp_path, monkeypatch):
    url = project(tmp_path, monkeypatch)
    proc = start(tmp_path, url)
    try:
        base = listening(proc)
        assert get(f"{base}/healthz")[0] == 200
        assert get(f"{base}/readyz")[0] == 200
        status, headers, _ = get(f"{base}/admin/api/resources")
        assert status == 401 and len(headers["x-request-id"]) > 0
    finally:
        proc.send_signal(signal.SIGTERM)
    code, err = run_to_end(proc)
    assert code == 0, err
    assert '"request_id"' in err


def test_monorm_admin_fails_with_a_clear_error_when_the_admin_tables_are_absent(tmp_path, monkeypatch):
    url = project(tmp_path, monkeypatch, migrate=False)
    code, err = run_to_end(start(tmp_path, url))
    assert code == 1
    assert err.startswith("error: the admin table ") and "run makemigrations and migrate" in err, err


def test_monorm_admin_exits_2_without_a_long_enough_secret(tmp_path, monkeypatch):
    url = project(tmp_path, monkeypatch)
    for secret in ("", "short"):
        code, err = run_to_end(start(tmp_path, url, MONORM_ADMIN_SECRET=secret))
        assert code == 2 and "MONORM_ADMIN_SECRET" in err, err
    environ = {k: v for k, v in os.environ.items() if k != "MONORM_ADMIN_SECRET"}
    proc = subprocess.Popen([sys.executable, "-m", "monorm", "admin"], cwd=tmp_path, env={**environ, "MONORM_DATABASE_URL": url}, stderr=subprocess.PIPE, text=True)
    code, err = run_to_end(proc)
    assert code == 2 and "MONORM_ADMIN_SECRET" in err, err


def test_monorm_admin_names_the_wrong_setting_and_the_missing_database(tmp_path, monkeypatch):
    url = project(tmp_path, monkeypatch)
    code, err = run_to_end(start(tmp_path, ""))
    assert code == 1 and "MONORM_DATABASE_URL" in err, err
    (tmp_path / "monorm.toml").write_text('[admin]\napp = "admin:admin"\nthreads = 4\n')
    code, err = run_to_end(start(tmp_path, url))
    assert code == 2 and "`admin.threads` is not a setting" in err, err
    (tmp_path / "monorm.toml").write_text('[admin]\napp = "admin:nothing"\n')
    code, err = run_to_end(start(tmp_path, url))
    assert code == 1 and "admin.app `admin:nothing`" in err, err


def test_the_setup_hook_connects_the_database_instead_of_the_environment(tmp_path, monkeypatch):
    url = project(tmp_path, monkeypatch)
    (tmp_path / "hooks.py").write_text(f"import monorm\n\nasync def setup():\n    await monorm.connect({url!r})\n")
    (tmp_path / "monorm.toml").write_text('[admin]\napp = "admin:admin"\nsetup = "hooks:setup"\n')
    proc = start(tmp_path, "")
    try:
        assert get(f"{listening(proc)}/readyz")[0] == 200
    finally:
        proc.send_signal(signal.SIGTERM)
    assert run_to_end(proc)[0] == 0


def test_two_processes_run_side_by_side_on_one_database(tmp_path, monkeypatch):
    url = project(tmp_path, monkeypatch)
    first, second = start(tmp_path, url), start(tmp_path, url)
    try:
        a, b = listening(first), listening(second)
        assert a != b
        assert get(f"{a}/readyz")[0] == 200 and get(f"{b}/readyz")[0] == 200
    finally:
        first.send_signal(signal.SIGTERM)
        second.send_signal(signal.SIGTERM)
    assert run_to_end(first)[0] == 0 and run_to_end(second)[0] == 0
