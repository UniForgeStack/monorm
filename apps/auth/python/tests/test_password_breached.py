"""The `breached` rule against a local stub of the range API: no real network. The TypeScript twin is `password-breached.test.ts`."""

import hashlib
import logging
import socket
import threading
from collections.abc import Iterator
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import pytest

from monorm_auth import load_rules, password_help_texts, rules, validate_password

# SHA-1 of "password" is 5BAA61E4C9B93F3F0682250B6CF8331B7EE68FD8.
PREFIX = "5BAA6"
SUFFIX = "1E4C9B93F3F0682250B6CF8331B7EE68FD8"
RANGE = "\r\n".join(["0018A45C4D1DEF81644B54AB7F969B88D65:1", f"{SUFFIX}:3730471", "00D4F6E8FA6EECAD2A3AA415EEC418D38EC:0"]) + "\r\n"


class Stub:
    """A range server: it records each request and answers with `status` and `body`, after `delay` seconds."""

    def __init__(self, body: str = RANGE, status: int = 200, delay: float = 0.0) -> None:
        self.requests: list[tuple[str, dict[str, str]]] = []
        self.release = threading.Event()
        stub = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self) -> None:
                stub.requests.append((self.path, dict(self.headers)))
                if delay:
                    stub.release.wait(delay)
                data = body.encode()
                self.send_response(status)
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def log_message(self, *args: object) -> None:
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self) -> None:
        self.release.set()
        self.server.shutdown()
        self.server.server_close()


@pytest.fixture
def stubs() -> Iterator[list[Stub]]:
    made: list[Stub] = []
    yield made
    for stub in made:
        stub.close()


def serve(stubs: list[Stub], **kw: object) -> Stub:
    stub = Stub(**kw)  # type: ignore[arg-type]
    stubs.append(stub)
    return stub


def closed_port_url() -> str:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return f"http://127.0.0.1:{s.getsockname()[1]}"


async def codes(rule: object, password: str = "password") -> list[str]:
    return [e.code for e in await validate_password(password, rules=[rule])]  # type: ignore[list-item]


async def test_a_breached_password_is_refused_and_the_host_gets_only_five_hash_characters(stubs: list[Stub]) -> None:
    stub = serve(stubs)
    errors = await validate_password("password", rules=[rules.Breached(host=stub.url)])
    assert [(e.code, e.params) for e in errors] == [("password_breached", {"count": 3730471})]
    [(path, headers)] = stub.requests
    assert path == f"/range/{PREFIX}"
    assert headers["Add-Padding"] == "true"
    assert SUFFIX not in path and "password" not in path


async def test_a_password_that_is_not_in_the_answer_passes_and_a_padding_line_does_not_count(stubs: list[Stub]) -> None:
    stub = serve(stubs, body=f"00D4F6E8FA6EECAD2A3AA415EEC418D38EC:3\r\n{SUFFIX}:0\r\n")
    assert await codes(rules.Breached(host=stub.url)) == []
    assert await codes(rules.Breached(host=stub.url), "another long password") == []


async def test_the_count_must_be_above_the_threshold_and_a_host_slash_is_ignored(stubs: list[Stub]) -> None:
    stub = serve(stubs)
    assert await codes(rules.Breached(host=stub.url + "/", threshold=3730470)) == ["password_breached"]
    assert await codes(rules.Breached(host=stub.url, threshold=3730471)) == []
    assert {path for path, _ in stub.requests} == {f"/range/{PREFIX}"}


async def test_fail_open_accepts_on_a_timeout_a_network_error_and_a_bad_answer_and_logs_a_warning(
    stubs: list[Stub], caplog: pytest.LogCaptureFixture
) -> None:
    slow = serve(stubs, delay=5)
    broken = serve(stubs, status=500, body="oops")
    secret = "Tr0ub4dor&3"
    garbled = serve(stubs, body=f"{hashlib.sha1(secret.encode()).hexdigest().upper()[5:]}:many\r\n")
    for url in (slow.url, closed_port_url(), broken.url, garbled.url):
        caplog.clear()
        with caplog.at_level(logging.WARNING, logger="monorm_auth"):
            assert await codes(rules.Breached(host=url, timeout=0.3), secret) == []
        assert [r.levelno for r in caplog.records] == [logging.WARNING]
        assert secret not in caplog.text
        assert SUFFIX not in caplog.text and PREFIX not in caplog.text


async def test_fail_closed_refuses_on_a_timeout_a_network_error_and_a_bad_answer(stubs: list[Stub]) -> None:
    slow = serve(stubs, delay=5)
    broken = serve(stubs, status=500, body="oops")
    for url in (slow.url, closed_port_url(), broken.url):
        errors = await validate_password("password", rules=[rules.Breached(host=url, timeout=0.3, fail="closed")])
        assert [e.code for e in errors] == ["password_check_unavailable"]


async def test_a_count_line_that_is_not_plain_digits_is_a_check_that_did_not_finish(stubs: list[Stub]) -> None:
    for count in ("-5", "1_000", "\u0663\u0664", "many", ""):
        stub = serve(stubs, body=f"{SUFFIX}:{count}\r\n")
        assert await codes(rules.Breached(host=stub.url, fail="closed")) == ["password_check_unavailable"]
        assert await codes(rules.Breached(host=stub.url)) == []


async def test_fail_closed_still_accepts_a_good_answer(stubs: list[Stub]) -> None:
    stub = serve(stubs, body="00D4F6E8FA6EECAD2A3AA415EEC418D38EC:3\r\n")
    assert await codes(rules.Breached(host=stub.url, fail="closed")) == []


async def test_breached_is_not_in_the_default_rules_and_the_settings_build_it(
    stubs: list[Stub], tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)
    (tmp_path / "monorm.toml").write_text("")
    assert not any(isinstance(r, rules.Breached) for r in load_rules())
    stub = serve(stubs)
    (tmp_path / "monorm.toml").write_text(
        f"[auth]\npassword_rules = [{{rule = 'min_length', min = 12}}, {{rule = 'breached', host = '{stub.url}', fail = 'closed'}}]\n"
    )
    assert [e.code for e in await validate_password("password")] == ["password_too_short", "password_breached"]
    assert password_help_texts()[1] == "Your password can't be one that appeared in a known data breach."


async def test_a_wrong_breached_option_is_an_error_in_the_settings_and_in_code(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.chdir(tmp_path)
    (tmp_path / "monorm.toml").write_text("[auth]\npassword_rules = [{rule = 'breached', fail = 'shut'}]\n")
    with pytest.raises(ValueError, match=r"`auth.password_rules\[0\].fail` must be \"open\" or \"closed\""):
        load_rules()
    with pytest.raises(ValueError, match="timeout"):
        rules.Breached(timeout=0)
    with pytest.raises(ValueError, match="host"):
        rules.Breached(host="pwned.local")
