"""A fake OpenID Connect provider for the admin tests: discovery, a key set, a token endpoint with PKCE, RS256 ID tokens.

It runs on a loopback port in a thread. A test reads the `Location` of `<base>/auth/start`, calls `approve` with the claims
of the user, and sends the browser to the callback it returns. The signing key is a fixed 2048-bit test key.
"""

from __future__ import annotations

import base64
import hashlib
import json
import secrets
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any
from urllib.parse import parse_qs, urlsplit

N = 20258116963081232338763807820206060290704176697405162328698962555082002150913140487203684079858727942986065409120198751027796477759276881899243892917401143165670483772470392202684268137983935795203290334082176269456607955684032123182053984211720101427990244467773114819828609010679878370072368749832855932003291226560947128930372575087401950579814363598976066219374029941585420029017659862713793203866461438333345454875948349186837291046793211209367662517413118215941118928291861518962392848824688411396053489612710529540538351022158447554923912922926323168729914356234382115757219612550574584232144866628301342064129
D = 2647575387057966266184015257763323629907732386937528157717951652420818704288104945241834201921611424889749015670781731475017900291761392388228641583935187116760943771078489824293013167826653759075769650626915891439554601346206709803746830804544272020603747128704213252833798835436088594525202309185804200175896845286959887369580230634838516327830248711028841739891237143575548909472089335983792785089827381490669160790993526995447537402444342919638684673109544143787995803279332066561470254931286164743084196687817450248579976879729257892784481639315040356670345126634229516564651025984853322996526977769288914543283
E = 65537
KID = "test-key"
CLIENT_ID = "monorm-admin"
CLIENT_SECRET = "idp-client-secret"
_SHA256_PREFIX = bytes.fromhex("3031300d060960864801650304020105000420")


def b64(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def _int_b64(n: int) -> str:
    return b64(n.to_bytes((n.bit_length() + 7) // 8, "big"))


def sign_rs256(signing_input: bytes) -> bytes:
    size = (N.bit_length() + 7) // 8
    digest = _SHA256_PREFIX + hashlib.sha256(signing_input).digest()
    padded = b"\x00\x01" + b"\xff" * (size - len(digest) - 3) + b"\x00" + digest
    return pow(int.from_bytes(padded, "big"), D, N).to_bytes(size, "big")


def jwt(claims: dict[str, Any], *, header: dict[str, Any] | None = None, sign: bool = True) -> str:
    head = {"alg": "RS256", "typ": "JWT", "kid": KID, **(header or {})}
    body = f"{b64(json.dumps(head).encode())}.{b64(json.dumps(claims).encode())}"
    signature = sign_rs256(body.encode()) if sign else b"bad"
    return f"{body}.{b64(signature)}"


class FakeIdp:
    """`issuer` is the base URL. `claims` are merged into the next tokens; `fail_token` makes the token endpoint answer 500."""

    def __init__(self) -> None:
        self.codes: dict[str, dict[str, Any]] = {}
        self.requests: list[tuple[str, str]] = []
        self.fail_token = False
        self.token_overrides: dict[str, Any] = {}
        idp = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args: Any) -> None:
                pass

            def _send(self, status: int, body: Any) -> None:
                data = json.dumps(body).encode()
                self.send_response(status)
                self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def do_GET(self) -> None:
                idp.requests.append(("GET", self.path))
                if self.path == "/.well-known/openid-configuration":
                    self._send(
                        200,
                        {
                            "issuer": idp.issuer,
                            "authorization_endpoint": f"{idp.issuer}/authorize",
                            "token_endpoint": f"{idp.issuer}/token",
                            "jwks_uri": f"{idp.issuer}/jwks",
                        },
                    )
                elif self.path == "/jwks":
                    self._send(200, {"keys": [{"kty": "RSA", "kid": KID, "use": "sig", "alg": "RS256", "n": _int_b64(N), "e": _int_b64(E)}]})
                else:
                    self._send(404, {})

            def do_POST(self) -> None:
                idp.requests.append(("POST", self.path))
                form = {k: v[0] for k, v in parse_qs(self.rfile.read(int(self.headers.get("content-length", 0))).decode()).items()}
                if self.path != "/token" or idp.fail_token:
                    self._send(500 if idp.fail_token else 404, {"error": "server_error"})
                    return
                grant = idp.codes.pop(form.get("code", ""), None)
                challenge = b64(hashlib.sha256(form.get("code_verifier", "").encode()).digest())
                ok = (
                    grant is not None
                    and form.get("client_id") == CLIENT_ID
                    and form.get("client_secret") == CLIENT_SECRET
                    and form.get("redirect_uri") == grant["redirect_uri"]
                    and challenge == grant["challenge"]
                )
                if not ok or grant is None:
                    self._send(400, {"error": "invalid_grant"})
                    return
                now = int(time.time())
                claims = {"iss": idp.issuer, "aud": CLIENT_ID, "sub": grant["sub"], "iat": now, "exp": now + 300, "nonce": grant["nonce"], **grant["claims"], **idp.token_overrides}
                self._send(200, {"access_token": secrets.token_urlsafe(8), "token_type": "Bearer", "id_token": jwt({k: v for k, v in claims.items() if v is not None})})

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.issuer = f"http://127.0.0.1:{self._server.server_address[1]}"
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)

    def __enter__(self) -> FakeIdp:
        self._thread.start()
        return self

    def __exit__(self, *exc: object) -> None:
        self._server.shutdown()
        self._server.server_close()

    def settings(self, **extra: Any) -> dict[str, Any]:
        """The `oidc` setting that points the admin at this provider."""
        return {"oidc": {"issuer": self.issuer, "client_id": CLIENT_ID, "client_secret": CLIENT_SECRET, **extra}}

    def approve(self, location: str, *, sub: str = "u-1", state: str | None = None, **claims: Any) -> str:
        """The user signs in at the provider: returns the callback path with the code and the state."""
        query = {k: v[0] for k, v in parse_qs(urlsplit(location).query).items()}
        assert query["response_type"] == "code" and query["code_challenge_method"] == "S256"
        code = secrets.token_urlsafe(16)
        self.codes[code] = {"nonce": query["nonce"], "challenge": query["code_challenge"], "redirect_uri": query["redirect_uri"], "sub": sub, "claims": claims}
        callback = urlsplit(query["redirect_uri"])
        return f"{callback.path}?code={code}&state={state or query['state']}"
