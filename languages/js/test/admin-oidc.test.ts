/** OIDC single sign-on of the admin: the authorization-code flow against a fake provider, roles from the claim, the audit rows. */

import assert from "node:assert/strict";
import { createHash, generateKeyPairSync, randomBytes, sign, type KeyObject } from "node:crypto";
import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { Admin, type AdminResponse, type AdminTestClient } from "../src/admin.js";
import { UserMethods, UserQueries, makePassword } from "../src/auth/index.js";
import { connect, load, Registry, useModelMethods, useQuerySet, type Database } from "../src/index.js";

const PG_URL = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
const SECRET = "s".repeat(40);
const ME = "/admin/api/me";
const START = "/admin/auth/start";
const PROVIDERS = ["sqlite", "postgres"] as const;
const CLIENT_ID = "monorm-admin";
const CLIENT_SECRET = "idp-client-secret";
const KID = "test-key";

type Loose = any;
interface World {
  readonly site: Admin;
  readonly db: Database;
  readonly models: Record<string, Loose>;
}

const { privateKey, publicKey } = generateKeyPairSync("rsa", { modulusLength: 2048 });

const b64 = (data: Buffer | string): string => Buffer.from(data).toString("base64url");

function jwt(claims: Record<string, unknown>, key: KeyObject = privateKey): string {
  const body = `${b64(JSON.stringify({ alg: "RS256", typ: "JWT", kid: KID }))}.${b64(JSON.stringify(claims))}`;
  return `${body}.${b64(sign("sha256", Buffer.from(body), key))}`;
}

interface Grant {
  nonce: string;
  challenge: string;
  redirectUri: string;
  sub: string;
  claims: Record<string, unknown>;
}

/** A fake OpenID Connect provider on a loopback port: discovery, a key set, a token endpoint with PKCE, RS256 ID tokens. */
class FakeIdp {
  readonly codes = new Map<string, Grant>();
  failToken = false;
  tokenOverrides: Record<string, unknown> = {};
  issuer = "";
  readonly #server: Server;

  constructor() {
    this.#server = createServer((req, res) => {
      const chunks: Buffer[] = [];
      req.on("data", (c: Buffer) => chunks.push(c));
      req.on("end", () => this.#handle(req.method ?? "GET", req.url ?? "/", Buffer.concat(chunks).toString(), (status, body) => {
        const data = JSON.stringify(body);
        res.writeHead(status, { "content-type": "application/json", "content-length": Buffer.byteLength(data) });
        res.end(data);
      }));
    });
  }

  async start(): Promise<this> {
    await new Promise<void>((resolve) => this.#server.listen(0, "127.0.0.1", resolve));
    this.issuer = `http://127.0.0.1:${(this.#server.address() as AddressInfo).port}`;
    return this;
  }

  async stop(): Promise<void> {
    this.#server.closeAllConnections();
    await new Promise<void>((resolve) => this.#server.close(() => resolve()));
  }

  #handle(method: string, path: string, raw: string, send: (status: number, body: unknown) => void): void {
    if (method === "GET") {
      if (path === "/.well-known/openid-configuration") {
        send(200, { issuer: this.issuer, authorization_endpoint: `${this.issuer}/authorize`, token_endpoint: `${this.issuer}/token`, jwks_uri: `${this.issuer}/jwks` });
      } else if (path === "/jwks") {
        send(200, { keys: [{ ...publicKey.export({ format: "jwk" }), kid: KID, use: "sig", alg: "RS256" }] });
      } else send(404, {});
      return;
    }
    const form = Object.fromEntries(new URLSearchParams(raw));
    if (path !== "/token" || this.failToken) return send(this.failToken ? 500 : 404, { error: "server_error" });
    const grant = this.codes.get(form["code"] ?? "");
    this.codes.delete(form["code"] ?? "");
    const challenge = b64(createHash("sha256").update(form["code_verifier"] ?? "").digest());
    const ok =
      grant !== undefined &&
      form["client_id"] === CLIENT_ID &&
      form["client_secret"] === CLIENT_SECRET &&
      form["redirect_uri"] === grant.redirectUri &&
      challenge === grant.challenge;
    if (!ok || grant === undefined) return send(400, { error: "invalid_grant" });
    const now = Math.floor(Date.now() / 1000);
    const claims: Record<string, unknown> = { iss: this.issuer, aud: CLIENT_ID, sub: grant.sub, iat: now, exp: now + 300, nonce: grant.nonce, ...grant.claims, ...this.tokenOverrides };
    for (const k of Object.keys(claims)) if (claims[k] === null) delete claims[k];
    send(200, { access_token: randomBytes(8).toString("base64url"), token_type: "Bearer", id_token: jwt(claims) });
  }

  /** The `oidc` setting that points the admin at this provider. */
  settings(extra: Record<string, unknown> = {}): Record<string, unknown> {
    return { oidc: { issuer: this.issuer, client_id: CLIENT_ID, client_secret: CLIENT_SECRET, ...extra } };
  }

  /** The user signs in at the provider: returns the callback path with the code and the state. */
  approve(location: string, options: { sub?: string; state?: string; [claim: string]: unknown } = {}): string {
    const { sub = "u-1", state, ...claims } = options;
    const query = Object.fromEntries(new URL(location).searchParams);
    assert.ok(query["response_type"] === "code" && query["code_challenge_method"] === "S256");
    const code = randomBytes(16).toString("base64url");
    this.codes.set(code, { nonce: query["nonce"]!, challenge: query["code_challenge"]!, redirectUri: query["redirect_uri"]!, sub, claims });
    const callback = new URL(query["redirect_uri"]!);
    return `${callback.pathname}?code=${code}&state=${state ?? query["state"]}`;
  }
}

async function world(provider: (typeof PROVIDERS)[number], run: (w: World, idp: FakeIdp) => Promise<void>): Promise<void> {
  const dir = mkdtempSync(join(tmpdir(), "monorm-admin-oidc-"));
  writeFileSync(join(dir, "package.json"), "{}");
  writeFileSync(
    join(dir, "schema.prisma"),
    `datasource db {\n  provider = "${provider}"\n}\nimport "app:auth" (prefix: "auth_")\nimport "app:admin" (prefix: "admin_")\n`,
  );
  const registry = new Registry();
  const models = load(join(dir, "schema.prisma"), { registry }) as unknown as Record<string, Loose>;
  useModelMethods(models["User"], UserMethods);
  useQuerySet(models["User"], UserQueries);
  let db: Database;
  try {
    db = await connect(provider === "sqlite" ? "sqlite://:memory:" : PG_URL, { registry, default: false });
  } catch (error) {
    rmSync(dir, { recursive: true, force: true });
    if (process.env["ORM_TEST_DATABASE_URL"] !== undefined) throw error;
    return;
  }
  const idp = await new FakeIdp().start();
  try {
    await db.dropTables();
    await db.createTables();
    const password = makePassword("pw");
    await models["User"].objects.using(db).insertMany([
      { username: "ann@example.com", email: "ann@example.com", password },
      { username: "off@example.com", email: "off@example.com", password, isActive: false },
    ]);
    await models["Role"].objects.using(db).insertMany([{ name: "editor" }, { name: "auditor" }]);
    await run({ site: new Admin(registry), db, models }, idp);
  } finally {
    await idp.stop();
    await db.dropTables();
    await db.close();
    rmSync(dir, { recursive: true, force: true });
  }
}

async function audit(db: Database, models: Record<string, Loose>): Promise<[string, string, Loose][]> {
  const entry = models["AdminLogEntry"];
  const rows = await entry.objects.using(db).orderBy(entry.id);
  return rows.map((r: Loose) => [r.action, r.recordLabel, typeof r.changes === "string" ? JSON.parse(r.changes) : r.changes]);
}

async function rolesOf(db: Database, models: Record<string, Loose>, username: string): Promise<string[]> {
  const user = await models["User"].objects.using(db).filter(models["User"].username.eq(username)).first();
  assert.ok(user);
  const link = models["UserRole"];
  const role = models["Role"];
  const ids = (await link.objects.using(db).filter(link.userId.eq(user.id))).map((j: Loose) => j.roleId);
  return ids.length === 0 ? [] : (await role.objects.using(db).filter(role.id.in(ids))).map((r: Loose) => r.name).sort();
}

const location = (reply: AdminResponse): string => {
  const value = reply.header("location");
  assert.ok(value !== null, reply.text);
  return value;
};

/** The browser round trip: the start, the provider, the callback. Returns the reply of the callback. */
async function signIn(client: AdminTestClient, idp: FakeIdp, claims: Record<string, unknown> = {}): Promise<AdminResponse> {
  const start = await client.get(START);
  assert.equal(start.status, 302, start.text);
  return client.get(idp.approve(location(start), claims));
}

const siteOf = ({ site, db }: World, idp: FakeIdp, extra: Record<string, unknown> = {}): AdminTestClient =>
  site.testClient(db, { secret: SECRET, settings: idp.settings(extra) });

for (const provider of PROVIDERS) {
  test(`the start redirects to the provider with state nonce and pkce (${provider})`, () => world(provider, async (w, idp) => {
    const client = siteOf(w, idp, { scopes: ["groups"] });
    const reply = await client.get(START);
    assert.equal(reply.status, 302);
    const target = new URL(location(reply));
    assert.ok(target.origin === idp.issuer && target.pathname === "/authorize");
    const query = target.searchParams;
    assert.ok(query.get("client_id") === CLIENT_ID && query.get("response_type") === "code");
    assert.equal(query.get("scope"), "openid profile email groups");
    assert.ok(query.get("redirect_uri")!.endsWith("/admin/auth/callback"));
    assert.ok(query.get("code_challenge_method") === "S256" && query.get("state")!.length >= 22 && query.get("nonce")!.length >= 22);
    assert.ok(reply.cookie("__Secure-monorm-oidc") !== null);
  }));

  test(`the login page names the sso start only when oidc is set (${provider})`, () => world(provider, async (w, idp) => {
    const plain = w.site.testClient(w.db, { secret: SECRET });
    assert.ok(!("sso" in (await plain.get("/admin/api/login")).json()));
    assert.equal((await siteOf(w, idp).get("/admin/api/login")).json().sso, START);
  }));

  test(`the login page names auto start only when the setting is on (${provider})`, () => world(provider, async (w, idp) => {
    assert.ok(!("auto_start" in (await siteOf(w, idp).get("/admin/api/login")).json()));
    assert.ok(!("auto_start" in (await siteOf(w, idp, { auto_start: false }).get("/admin/api/login")).json()));
    const body = (await siteOf(w, idp, { auto_start: true }).get("/admin/api/login")).json();
    assert.deepEqual([body.sso, body.auto_start], [START, true]);
  }));

  test(`the user returns to the next path and a foreign path is dropped (${provider})`, () => world(provider, async (w, idp) => {
    const client = siteOf(w, idp);
    const start = await client.get(`${START}?next=/admin/-/resources/Book%3Fpage%3D2`);
    const done = await client.get(idp.approve(location(start), { email: "ann@example.com" }));
    assert.deepEqual([done.status, location(done)], [302, "/admin/-/resources/Book?page=2"]);
    for (const foreign of ["https://evil.example/", "//evil.example/", "/other/page", "/admin/../x", "/admin/.%2e/x", "/admin/%2e./x", "/admin/auth/start"]) {
      const other = siteOf(w, idp);
      const first = await other.get(`${START}?next=${foreign}`);
      const last = await other.get(idp.approve(location(first), { email: "ann@example.com" }));
      assert.deepEqual([last.status, location(last)], [302, "/admin/"], foreign);
    }
  }));

  test(`a login of a new user creates it and sets the roles of the claim (${provider})`, () => world(provider, async (w, idp) => {
    const client = siteOf(w, idp);
    const done = await signIn(client, idp, { email: "new@example.com", roles: ["editor", "nobody"] });
    assert.deepEqual([done.status, location(done)], [302, "/admin/"]);
    const me = await client.get(ME);
    assert.deepEqual([me.status, me.json().user.username], [200, "new@example.com"]);
    assert.deepEqual(await rolesOf(w.db, w.models, "new@example.com"), ["editor"]);
    const User = w.models["User"];
    const created = await User.objects.using(w.db).filter(User.username.eq("new@example.com")).first();
    assert.ok(created !== null && created.email === "new@example.com" && !created.isSuperuser);
    assert.ok(!created.checkPassword(""));
    const rows = await audit(w.db, w.models);
    assert.equal(rows.length, 1);
    const [action, label, changes] = rows[0]!;
    assert.deepEqual([action, label], ["login", "new@example.com"]);
    assert.ok(changes.method === "sso" && changes.created === true);
  }));

  test(`the roles follow the claim at each login and a user without a match has none (${provider})`, () => world(provider, async (w, idp) => {
    const client = siteOf(w, idp);
    await signIn(client, idp, { email: "ann@example.com", roles: ["editor", "auditor"] });
    assert.deepEqual(await rolesOf(w.db, w.models, "ann@example.com"), ["auditor", "editor"]);
    await client.logout();
    await signIn(client, idp, { email: "ann@example.com", roles: ["auditor"] });
    assert.deepEqual(await rolesOf(w.db, w.models, "ann@example.com"), ["auditor"]);
    await client.logout();
    await signIn(client, idp, { email: "ann@example.com", roles: ["unknown"] });
    assert.deepEqual(await rolesOf(w.db, w.models, "ann@example.com"), []);
    await client.logout();
    await signIn(client, idp, { email: "ann@example.com" });
    assert.deepEqual(await rolesOf(w.db, w.models, "ann@example.com"), []);
    assert.deepEqual((await audit(w.db, w.models)).map(([a]) => a).filter((a) => a === "login"), ["login", "login", "login", "login"]);
    assert.equal((await client.get(ME)).status, 200);
  }));

  test(`the roles claim can be one name or a nested path (${provider})`, () => world(provider, async (w, idp) => {
    let client = siteOf(w, idp, { roles_claim: "realm.roles" });
    await signIn(client, idp, { email: "ann@example.com", realm: { roles: ["editor"] } });
    assert.deepEqual(await rolesOf(w.db, w.models, "ann@example.com"), ["editor"]);
    await client.logout();
    client = siteOf(w, idp);
    await signIn(client, idp, { email: "ann@example.com", roles: "auditor" });
    assert.deepEqual(await rolesOf(w.db, w.models, "ann@example.com"), ["auditor"]);
  }));

  test(`an inactive user cannot log in and its roles stay (${provider})`, () => world(provider, async (w, idp) => {
    const client = siteOf(w, idp);
    const done = await signIn(client, idp, { email: "off@example.com", roles: ["editor"] });
    assert.equal(location(done), "/admin/?sso_error=inactive");
    assert.equal((await client.get(ME)).status, 401);
    assert.deepEqual(await rolesOf(w.db, w.models, "off@example.com"), []);
    const rows = await audit(w.db, w.models);
    assert.equal(rows.length, 1);
    const [action, label, changes] = rows[0]!;
    assert.deepEqual([action, label, changes.reason], ["login_failed", "off@example.com", "inactive"]);
  }));

  test(`a wrong state is refused and writes a failed login row (${provider})`, () => world(provider, async (w, idp) => {
    const client = siteOf(w, idp);
    const done = await signIn(client, idp, { state: "not-the-state", email: "ann@example.com" });
    assert.equal(location(done), "/admin/?sso_error=failed");
    assert.equal((await client.get(ME)).status, 401);
    const rows = await audit(w.db, w.models);
    assert.equal(rows.length, 1);
    assert.deepEqual([rows[0]![0], rows[0]![2].reason], ["login_failed", "bad_state"]);
  }));

  test(`a callback without a started flow is refused (${provider})`, () => world(provider, async (w, idp) => {
    const client = siteOf(w, idp);
    const done = await client.get("/admin/auth/callback?code=x&state=y");
    assert.equal(location(done), "/admin/?sso_error=failed");
  }));

  test(`a code works once (${provider})`, () => world(provider, async (w, idp) => {
    const client = siteOf(w, idp);
    const start = await client.get(START);
    const callback = idp.approve(location(start), { email: "ann@example.com" });
    assert.equal(location(await client.get(callback)), "/admin/");
    await client.logout();
    const again = await client.get(callback);
    assert.equal(location(again), "/admin/?sso_error=failed");
    assert.equal((await client.get(ME)).status, 401);
  }));

  test(`a token with a wrong claim is refused (${provider})`, () => world(provider, async (w, idp) => {
    const overrides: Record<string, unknown>[] = [{ nonce: "other" }, { aud: "someone-else" }, { iss: "https://evil.example.com" }, { exp: 1 }, { email: null }];
    for (const [i, override] of overrides.entries()) {
      idp.tokenOverrides = override;
      const client = siteOf(w, idp);
      const done = await signIn(client, idp, { email: "ann@example.com", roles: ["editor"] });
      assert.equal(location(done), "/admin/?sso_error=failed", JSON.stringify(override));
      assert.equal((await client.get(ME)).status, 401);
      assert.deepEqual(await rolesOf(w.db, w.models, "ann@example.com"), []);
      assert.deepEqual((await audit(w.db, w.models)).map(([a]) => a), Array(i + 1).fill("login_failed"));
    }
  }));

  test(`the hosted domain is checked on the server (${provider})`, () => world(provider, async (w, idp) => {
    const client = siteOf(w, idp, { hd: "example.com" });
    assert.equal(location(await signIn(client, idp, { email: "ann@example.com", hd: "other.com" })), "/admin/?sso_error=domain");
    assert.equal(location(await signIn(client, idp, { email: "ann@example.com" })), "/admin/?sso_error=domain");
    assert.equal((await client.get(ME)).status, 401);
    assert.equal(location(await signIn(client, idp, { email: "ann@example.com", hd: "example.com" })), "/admin/");
    assert.equal((await client.get(ME)).status, 200);
  }));

  test(`a provider error and an unreachable provider send the browser back (${provider})`, () => world(provider, async (w, idp) => {
    const client = siteOf(w, idp);
    assert.equal(location(await client.get("/admin/auth/callback?error=access_denied")), "/admin/?sso_error=denied");
    idp.failToken = true;
    assert.equal(location(await signIn(client, idp, { email: "ann@example.com" })), "/admin/?sso_error=unavailable");
    assert.deepEqual((await audit(w.db, w.models)).map(([a]) => a), ["login_failed", "login_failed"]);
    assert.equal((await client.get(ME)).status, 401);
  }));

  test(`the flow ends with the callback (${provider})`, () => world(provider, async (w, idp) => {
    const client = siteOf(w, idp);
    await signIn(client, idp, { email: "ann@example.com" });
    assert.ok(!client.cookies.has("__Secure-monorm-oidc"));
  }));
}
