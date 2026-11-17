// A fake OpenID Connect provider for the browser smoke test: discovery, a key set, an authorize page that signs the user in
// at once, and a token endpoint that checks PKCE and the client secret. It runs on a loopback port.
import { createHash, generateKeyPairSync, randomBytes, sign } from "node:crypto";
import { createServer } from "node:http";

export const CLIENT_ID = "monorm-admin";
export const CLIENT_SECRET = "idp-client-secret";
const KID = "e2e-key";
const { privateKey, publicKey } = generateKeyPairSync("rsa", { modulusLength: 2048 });
const b64 = (data) => Buffer.from(data).toString("base64url");

function jwt(claims) {
  const body = `${b64(JSON.stringify({ alg: "RS256", typ: "JWT", kid: KID }))}.${b64(JSON.stringify(claims))}`;
  return `${body}.${b64(sign("sha256", Buffer.from(body), privateKey))}`;
}

/** `claims` are the claims of the user that signs in (a test may change them); `sub` is its subject. */
export async function startIdp({ port = 0, claims, sub = "u-1" }) {
  const codes = new Map();
  let issuer = "";
  const json = (res, status, body) => {
    res.writeHead(status, { "content-type": "application/json" }).end(JSON.stringify(body));
  };
  const server = createServer((req, res) => {
    const url = new URL(req.url, issuer);
    if (req.method === "GET" && url.pathname === "/.well-known/openid-configuration") {
      return json(res, 200, { issuer, authorization_endpoint: `${issuer}/authorize`, token_endpoint: `${issuer}/token`, jwks_uri: `${issuer}/jwks` });
    }
    if (req.method === "GET" && url.pathname === "/jwks") {
      return json(res, 200, { keys: [{ ...publicKey.export({ format: "jwk" }), kid: KID, use: "sig", alg: "RS256" }] });
    }
    if (req.method === "GET" && url.pathname === "/authorize") {
      const q = url.searchParams;
      const code = randomBytes(12).toString("base64url");
      codes.set(code, { nonce: q.get("nonce"), challenge: q.get("code_challenge"), redirectUri: q.get("redirect_uri") });
      const back = new URL(q.get("redirect_uri"));
      back.searchParams.set("code", code);
      back.searchParams.set("state", q.get("state"));
      res.writeHead(302, { location: back.toString() }).end();
      return;
    }
    if (req.method === "POST" && url.pathname === "/token") {
      const chunks = [];
      req.on("data", (c) => chunks.push(c));
      req.on("end", () => {
        const form = new URLSearchParams(Buffer.concat(chunks).toString());
        const grant = codes.get(form.get("code"));
        codes.delete(form.get("code"));
        const challenge = b64(createHash("sha256").update(form.get("code_verifier") ?? "").digest());
        const ok = grant && form.get("client_id") === CLIENT_ID && form.get("client_secret") === CLIENT_SECRET && form.get("redirect_uri") === grant.redirectUri && challenge === grant.challenge;
        if (!ok) return json(res, 400, { error: "invalid_grant" });
        const now = Math.floor(Date.now() / 1000);
        json(res, 200, { access_token: "a", token_type: "Bearer", id_token: jwt({ iss: issuer, aud: CLIENT_ID, sub, iat: now, exp: now + 300, nonce: grant.nonce, ...claims }) });
      });
      return;
    }
    json(res, 404, {});
  });
  await new Promise((resolve) => server.listen(port, "127.0.0.1", resolve));
  issuer = `http://127.0.0.1:${server.address().port}`;
  return { issuer, claims, close: () => new Promise((resolve) => server.close(resolve)) };
}
