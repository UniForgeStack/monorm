/** Password login, sessions and HTTP security of the admin, through `admin.testClient(db, { secret })`. */

import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { action, Admin, type AdminResponse, type AdminTestClient } from "../src/admin.js";
import { UserMethods, UserQueries, makePassword } from "../src/auth/index.js";
import { connect, load, Registry, useModelMethods, useQuerySet, type Database } from "../src/index.js";

const PG_URL = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
const SECRET = "s".repeat(40);
const OTHER_SECRET = "o".repeat(40);
const ME = "/admin/api/me";
const PASSWORD_URL = "/admin/api/password";
const PROVIDERS = ["sqlite", "postgres"] as const;

type Loose = any;
interface World {
  readonly site: Admin;
  readonly db: Database;
  readonly models: Record<string, Loose>;
}

async function world(provider: (typeof PROVIDERS)[number], run: (w: World) => Promise<void>): Promise<void> {
  const dir = mkdtempSync(join(tmpdir(), "monorm-admin-login-"));
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
  try {
    await db.dropTables();
    await db.createTables();
    const password = makePassword("pw");
    await models["User"].objects.using(db).insertMany([
      { username: "ann", email: "ann@example.com", password, isSuperuser: true },
      { username: "off", email: "off@example.com", password, isActive: false },
    ]);
    await run({ site: new Admin(registry), db, models });
  } finally {
    await db.dropTables();
    await db.close();
    rmSync(dir, { recursive: true, force: true });
  }
}

async function audit(db: Database, models: Record<string, Loose>): Promise<[string, string, string][]> {
  const entry = models["AdminLogEntry"];
  const rows = await entry.objects.using(db).orderBy(entry.id);
  return rows.map((r: Loose) => [r.userKey, r.action, r.recordLabel]);
}

const body = { old_password: "pw", new_password: "new-password" };

function kind(reply: AdminResponse): string {
  return reply.json().error.kind;
}

for (const provider of PROVIDERS) {
  test(`a login sets a signed http only cookie and writes an audit row (${provider})`, () => world(provider, async ({ site, db, models }) => {
    const client = site.testClient(db, { secret: SECRET });
    const reply = await client.login("ann", "pw");
    assert.equal(reply.status, 200, reply.text);
    const session = reply.cookie("__Secure-monorm-session");
    assert.ok(session);
    for (const flag of ["HttpOnly", "Secure", "SameSite=Lax", "Path=/admin/"]) assert.ok(session.includes(flag), flag);
    assert.ok(!session.includes("Domain"));
    assert.equal(reply.json().user.username, "ann");
    assert.ok(!("password" in reply.json().user));
    const me = await client.get(ME);
    assert.deepEqual([me.status, me.json().user.username], [200, "ann"]);
    assert.deepEqual(await audit(db, models), [["1", "login", "ann"]]);
  }));

  test(`a wrong password and an unknown user give the same 401 and a failed login row (${provider})`, () => world(provider, async ({ site, db, models }) => {
    const client = site.testClient(db, { secret: SECRET });
    const wrong = await client.login("ann", "nope");
    const unknown = await client.login("zed", "nope");
    assert.deepEqual([wrong.status, unknown.status], [401, 401]);
    assert.deepEqual(wrong.json(), unknown.json());
    // the typed text is no account: the user key stays empty and the label keeps the name
    assert.deepEqual(await audit(db, models), [["", "login_failed", "ann"], ["", "login_failed", "zed"]]);
  }));

  test(`an inactive user cannot log in (${provider})`, () => world(provider, async ({ site, db }) => {
    const client = site.testClient(db, { secret: SECRET });
    assert.equal((await client.login("off", "pw")).status, 401);
    assert.equal((await client.get(ME)).status, 401);
  }));

  test(`a request without a session gets 401 before 404 (${provider})`, () => world(provider, async ({ site, db }) => {
    const client = site.testClient(db, { secret: SECRET });
    for (const path of ["/admin/api/resources", "/admin/api/nothing", "/elsewhere", ME]) assert.equal((await client.get(path)).status, 401, path);
    await client.login("ann", "pw");
    assert.equal((await client.get("/admin/api/nothing")).status, 404);
    assert.equal((await client.get("/admin/api/resources")).status, 200);
  }));

  test(`five failures for a user from one ip answer 429 but another ip still signs in (${provider})`, () => world(provider, async ({ site, db }) => {
    const client = site.testClient(db, { secret: SECRET, ip: "10.0.1.1" });
    for (let i = 0; i < 5; i++) assert.equal((await client.login("ann", "nope")).status, 401);
    const blocked = await client.login("ann", "pw");
    assert.equal(blocked.status, 429);
    assert.ok(blocked.header("retry-after"));
    assert.equal((await client.login("other", "nope")).status, 401);
    client.ip = "10.0.1.9";
    assert.equal((await client.login("ann", "pw")).status, 200, "an attacker cannot lock the owner out from one address");
  }));

  test(`fifty failures for one name over many ips answer 429 for every ip (${provider})`, () => world(provider, async ({ site, db }) => {
    const client = site.testClient(db, { secret: SECRET });
    for (let i = 0; i < 10; i++) {
      client.ip = `10.0.4.${i}`;
      for (let j = 0; j < 5; j++) assert.equal((await client.login("ann", "nope")).status, 401);
    }
    client.ip = "10.0.4.99";
    assert.equal((await client.login("ann", "pw")).status, 429);
    assert.equal((await client.login("other", "nope")).status, 401);
  }));

  test(`a blocked login writes a login blocked row (${provider})`, () => world(provider, async ({ site, db, models }) => {
    const client = site.testClient(db, { secret: SECRET, ip: "10.0.6.1" });
    for (let i = 0; i < 5; i++) await client.login("ann", "nope");
    for (let i = 0; i < 3; i++) assert.equal((await client.login("ann", "pw")).status, 429);
    const blocked = (await audit(db, models)).filter((row) => row[1] === "login_blocked");
    assert.deepEqual(blocked, [["", "login_blocked", "ann"]], "three attempts, one row");
  }));

  test(`x forwarded for counts only from a trusted proxy (${provider})`, () => world(provider, async ({ site, db }) => {
    const through = async (client: AdminTestClient, forwarded: string, password: string): Promise<AdminResponse> => {
      await client.get("/admin/api/login");
      return client.post("/admin/api/login", { json: { username: "ann", password }, headers: { "X-Forwarded-For": forwarded } });
    };
    const plain = site.testClient(db, { secret: SECRET, ip: "10.0.5.1" });
    for (let i = 0; i < 20; i++) await plain.login(`user${i}`, "nope");
    assert.equal((await through(plain, "198.51.100.200", "pw")).status, 429, "an untrusted peer cannot choose its address");
    const proxied = site.testClient(db, { secret: SECRET, ip: "10.0.5.9", settings: { trusted_proxies: ["10.0.5.9"] } });
    for (let i = 0; i < 5; i++) assert.equal((await through(proxied, "203.0.113.5", "nope")).status, 401);
    assert.equal((await through(proxied, "203.0.113.5", "pw")).status, 429);
    assert.equal((await through(proxied, "203.0.113.6", "pw")).status, 200);
  }));

  test(`a trusted proxy that is no address is a value error (${provider})`, () => world(provider, async ({ site, db }) => {
    await assert.rejects(() => site.testClient(db, { secret: SECRET, settings: { trusted_proxies: ["not-an-ip"] } }).get(ME), /trusted_proxies/);
  }));

  test(`twenty failures from one ip answer 429 for any user (${provider})`, () => world(provider, async ({ site, db }) => {
    const client = site.testClient(db, { secret: SECRET, ip: "10.0.2.2" });
    for (let i = 0; i < 20; i++) assert.equal((await client.login(`user${i}`, "nope")).status, 401);
    assert.equal((await client.login("ann", "pw")).status, 429);
    client.ip = "10.0.2.3";
    assert.equal((await client.login("ann", "pw")).status, 200);
  }));

  test(`a write needs the csrf token the origin and json (${provider})`, () => world(provider, async ({ site, db }) => {
    const client = site.testClient(db, { secret: SECRET });
    await client.login("ann", "pw");
    const noToken = await client.post(PASSWORD_URL, { json: body, csrf: null });
    assert.deepEqual([noToken.status, kind(noToken)], [403, "BadCsrfToken"]);
    assert.equal((await client.post(PASSWORD_URL, { json: body, csrf: "abc.def" })).status, 403);
    const noOrigin = await client.post(PASSWORD_URL, { json: body, origin: null });
    assert.deepEqual([noOrigin.status, kind(noOrigin)], [403, "BadOrigin"]);
    assert.equal((await client.post(PASSWORD_URL, { json: body, origin: "https://evil.example" })).status, 403);
    const referer = await client.post(PASSWORD_URL, { json: body, origin: null, headers: { Referer: "http://localhost/admin/x" } });
    assert.equal(referer.status, 200);
    const form = await client.post(PASSWORD_URL, { headers: { "Content-Type": "application/x-www-form-urlencoded" } });
    assert.deepEqual([form.status, kind(form)], [415, "UnsupportedMediaType"]);
    const other = site.testClient(db, { secret: SECRET, ip: "10.0.3.3" });
    await other.login("ann", "pw");
    const swapped = await client.post(PASSWORD_URL, { json: body, csrf: other.cookies.get("__Secure-monorm-csrf")! });
    assert.equal(swapped.status, 403, "a token is bound to its session");
  }));

  test(`a configured origin is accepted and the login needs csrf too (${provider})`, () => world(provider, async ({ site, db }) => {
    const client = site.testClient(db, { secret: SECRET, settings: { origins: ["https://admin.example.com"] } });
    await client.get("/admin/api/login");
    const credentials = { username: "ann", password: "pw" };
    assert.equal((await client.post("/admin/api/login", { json: credentials, csrf: null })).status, 403);
    assert.equal((await client.post("/admin/api/login", { json: credentials, origin: "https://admin.example.com" })).status, 200);
  }));

  test(`every response has the security headers (${provider})`, () => world(provider, async ({ site, db }) => {
    const client = site.testClient(db, { secret: SECRET });
    const replies = [await client.get("/admin/api/resources"), await client.get("/healthz"), await client.get("/admin/api/login")];
    await client.login("ann", "pw");
    replies.push(await client.get("/admin/api/nothing"), await client.get("/admin/api/resources"), await client.post(PASSWORD_URL));
    for (const reply of replies) {
      assert.ok(reply.header("content-security-policy")?.includes("default-src 'none'"));
      assert.equal(reply.header("x-frame-options"), "DENY");
      assert.equal(reply.header("referrer-policy"), "same-origin");
      assert.equal(reply.header("cache-control"), "no-store");
    }
  }));

  test(`two clients with the same secret share a session and another secret does not (${provider})`, () => world(provider, async ({ site, db }) => {
    const first = site.testClient(db, { secret: SECRET });
    await first.login("ann", "pw");
    const share = (client: AdminTestClient): AdminTestClient => {
      for (const [k, v] of first.cookies) client.cookies.set(k, v);
      return client;
    };
    assert.equal((await share(site.testClient(db, { secret: SECRET, ip: "10.0.4.4" })).get(ME)).status, 200);
    assert.equal((await share(site.testClient(db, { secret: OTHER_SECRET, ip: "10.0.4.5" })).get(ME)).status, 401);
    const tampered = share(site.testClient(db, { secret: SECRET, ip: "10.0.4.6" }));
    tampered.cookies.set("__Secure-monorm-session", `${first.cookies.get("__Secure-monorm-session")}x`);
    assert.equal((await tampered.get(ME)).status, 401);
  }));

  test(`the idle timeout ends a session (${provider})`, () => world(provider, async ({ site, db }) => {
    const client = site.testClient(db, { secret: SECRET, settings: { idle_timeout: 1 } });
    await client.login("ann", "pw");
    assert.equal((await client.get(ME)).status, 200);
    client.advanceClock(3);
    assert.equal((await client.get(ME)).status, 401);
  }));

  test(`the absolute timeout ends a session that is in use (${provider})`, () => world(provider, async ({ site, db }) => {
    const client = site.testClient(db, { secret: SECRET, settings: { session_timeout: 2 } });
    await client.login("ann", "pw");
    for (let i = 0; i < 2; i++) {
      assert.equal((await client.get(ME)).status, 200);
      client.advanceClock(1);
    }
    client.advanceClock(1);
    assert.equal((await client.get(ME)).status, 401);
  }));

  test(`logout ends the session even for a copy of the cookie (${provider})`, () => world(provider, async ({ site, db }) => {
    const client = site.testClient(db, { secret: SECRET });
    await client.login("ann", "pw");
    const copy = new Map(client.cookies);
    assert.equal((await client.logout()).status, 200);
    assert.equal((await client.get(ME)).status, 401);
    for (const [k, v] of copy) client.cookies.set(k, v);
    assert.equal((await client.get(ME)).status, 401, "the revoked session stays revoked in this process");
  }));

  const RENEWS = { idle_timeout: 4 }; // a session is checked again every second (a quarter of the idle time)

  const changeAnn = async ({ db, models }: World, values: Record<string, unknown>): Promise<void> => {
    const User = models["User"];
    const ann = await User.objects.using(db).filter(User.username.eq("ann")).first();
    await ann.update(values);
  };

  test(`a deactivated user loses the session at the next renewal (${provider})`, () => world(provider, async (w) => {
    const client = w.site.testClient(w.db, { secret: SECRET, settings: RENEWS });
    await client.login("ann", "pw");
    await changeAnn(w, { isActive: false });
    assert.equal((await client.get(ME)).status, 200, "the change shows at the renewal, not before");
    client.advanceClock(2);
    assert.equal((await client.get(ME)).status, 401);
    assert.equal((await client.get(ME)).status, 401);
  }));

  test(`a demoted superuser loses the session at the next renewal (${provider})`, () => world(provider, async (w) => {
    const client = w.site.testClient(w.db, { secret: SECRET, settings: RENEWS });
    await client.login("ann", "pw");
    await changeAnn(w, { isSuperuser: false });
    client.advanceClock(2);
    assert.equal((await client.get(ME)).status, 401);
  }));

  test(`a password changed outside the service ends the session at the next renewal (${provider})`, () => world(provider, async (w) => {
    const client = w.site.testClient(w.db, { secret: SECRET, settings: RENEWS });
    await client.login("ann", "pw");
    await changeAnn(w, { password: makePassword("changed-elsewhere") });
    client.advanceClock(2);
    assert.equal((await client.get(ME)).status, 401);
  }));

  test(`a logout in one client ends the copy of the cookie in another client (${provider})`, () => world(provider, async ({ site, db }) => {
    const first = site.testClient(db, { secret: SECRET, settings: RENEWS });
    await first.login("ann", "pw");
    const second = site.testClient(db, { secret: SECRET, ip: "10.0.7.7", settings: RENEWS });
    for (const [k, v] of first.cookies) second.cookies.set(k, v);
    assert.equal((await second.get(ME)).status, 200);
    assert.equal((await first.logout()).status, 200);
    assert.equal((await second.get(ME)).status, 200, "the second client learns of the logout at its next renewal");
    second.advanceClock(2);
    assert.equal((await second.get(ME)).status, 401);
  }));

  test(`a password change ends the other sessions and keeps the changed one (${provider})`, () => world(provider, async ({ site, db }) => {
    const client = site.testClient(db, { secret: SECRET, settings: RENEWS });
    await client.login("ann", "pw");
    const old = new Map(client.cookies);
    const elsewhere = site.testClient(db, { secret: SECRET, ip: "10.0.8.8", settings: RENEWS });
    await elsewhere.login("ann", "pw");
    const done = await client.post(PASSWORD_URL, { json: { old_password: "pw", new_password: "new-password" } });
    assert.equal(done.status, 200);
    assert.ok(done.cookie("__Secure-monorm-session") && done.json().csrf);
    assert.notEqual(client.cookies.get("__Secure-monorm-session"), old.get("__Secure-monorm-session"));
    for (const [k, v] of old) client.cookies.set(k, v);
    assert.equal((await client.get(ME)).status, 401, "the old cookie is dead at once");
    await client.login("ann", "new-password");
    client.advanceClock(2);
    elsewhere.advanceClock(2);
    assert.equal((await client.get(ME)).status, 200, "the new session knows the new hash");
    assert.equal((await elsewhere.get(ME)).status, 401);
  }));

  test(`a password change needs the old password and writes an audit row (${provider})`, () => world(provider, async ({ site, db, models }) => {
    const client = site.testClient(db, { secret: SECRET });
    await client.login("ann", "pw");
    const wrong = await client.post(PASSWORD_URL, { json: { old_password: "nope", new_password: "new-password" } });
    assert.deepEqual([wrong.status, kind(wrong)], [400, "InvalidPassword"]);
    assert.equal((await client.post(PASSWORD_URL, { json: { new_password: "new-password" } })).status, 400);
    assert.equal((await client.post(PASSWORD_URL, { json: { old_password: "pw", new_password: "new-password" } })).status, 200);
    assert.deepEqual((await audit(db, models)).map((row) => row[1]), ["login", "password_change_failed", "password_change"]);
    assert.equal((await site.testClient(db, { secret: SECRET, ip: "10.0.6.6" }).login("ann", "pw")).status, 401);
    assert.equal((await site.testClient(db, { secret: SECRET, ip: "10.0.6.7" }).login("ann", "new-password")).status, 200);
  }));

  test(`a weak new password is refused with the rule codes and the audit row has no password (${provider})`, () => world(provider, async ({ site, db, models }) => {
    const client = site.testClient(db, { secret: SECRET });
    await client.login("ann", "pw");
    const weak = await client.post(PASSWORD_URL, { json: { old_password: "pw", new_password: "1234" } });
    const error = weak.json().error;
    assert.deepEqual([weak.status, error.kind], [400, "WeakPassword"]);
    assert.deepEqual(error.errors.map((e: Loose) => e.code), ["password_too_short", "password_too_common", "password_entirely_numeric"]);
    assert.deepEqual(error.errors[0].params, { min: 8 });
    assert.ok(error.message.startsWith("This password is too short."));
    const entry = models["AdminLogEntry"];
    const last = (await entry.objects.using(db).orderBy(entry.id.desc()))[0];
    assert.equal(last.action, "password_change_failed");
    const changes = JSON.stringify(last.changes);
    assert.ok(changes.includes("password_too_short") && !changes.includes("1234"), changes);
    assert.equal((await site.testClient(db, { secret: SECRET, ip: "10.0.6.8" }).login("ann", "pw")).status, 200);
  }));

  test(`weak new passwords do not use up the password limit but wrong old passwords do (${provider})`, () => world(provider, async ({ site, db }) => {
    const client = site.testClient(db, { secret: SECRET });
    await client.login("ann", "pw");
    for (let i = 0; i < 7; i++) {
      assert.equal((await client.post(PASSWORD_URL, { json: { old_password: "pw", new_password: "1234" } })).status, 400);
    }
    for (let i = 0; i < 5; i++) {
      const wrong = await client.post(PASSWORD_URL, { json: { old_password: "no", new_password: "a-long-new-password" } });
      assert.equal(wrong.json().error.kind, "InvalidPassword");
    }
    assert.equal((await client.post(PASSWORD_URL, { json: { old_password: "pw", new_password: "a-long-new-password" } })).status, 429);
  }));

  test(`the user client needs no login and does not mix with a secret (${provider})`, () => world(provider, async ({ site, db }) => {
    assert.equal((await site.testClient(db, { user: { is_superuser: true } }).get("/admin/api/resources")).status, 200);
    assert.equal((await site.testClient(db).get("/admin/api/resources")).status, 401);
    assert.throws(() => site.testClient(db, { user: { is_superuser: true }, secret: SECRET }), TypeError);
  }));

  test(`a missing or short secret is a value error (${provider})`, () => world(provider, async ({ site, db }) => {
    await assert.rejects(() => site.testClient(db, { secret: "short" }).get(ME), /MONORM_ADMIN_SECRET/);
  }));
}

for (const provider of PROVIDERS) {
  test(`set password needs the auth user model (${provider})`, () => world(provider, async ({ site, models }) => {
    site.register(models["AdminLogEntry"], { setPassword: true });
    assert.throws(() => site.check(), /set_password.*needs the auth user model/);
  }));

  test(`set password action cannot replace the resource change rule (${provider})`, () => world(provider, async ({ site, models }) => {
    const user = models["User"];
    const reset = action(async () => { throw new Error("Rust runs the built-in action"); }, {
      name: "set_password", placement: "detail", can: user.id.gt(0), replaceChange: true,
    });
    site.register(user, { setPassword: true, actions: [reset] });
    assert.throws(() => site.check(), /set_password always requires the resource change rule/);
  }));

  test(`set password keeps the current session and ends the other sessions (${provider})`, () => world(provider, async ({ site, db, models }) => {
    const user = models["User"];
    site.register(user, { setPassword: true });
    const actor = site.testClient(db, { secret: SECRET });
    const other = site.testClient(db, { secret: SECRET });
    assert.equal((await actor.login("ann", "pw")).status, 200);
    assert.equal((await other.login("ann", "pw")).status, 200);
    const entry = models["AdminLogEntry"];
    const users = user.objects.using(db);
    const getActor = () => users.filter(user.id.eq(1)).first();
    const oldHash = (await getActor()).password;
    async function reset(first: string, second: string, statements: number, keys = ["1"]) {
      let queries = 0;
      const stop = db.onQuery(() => { queries++; });
      try {
        const reply = await actor.post("/admin/api/actions/User/set_password", {
          json: { keys, inputs: { new_password: first, new_password_again: second } },
        });
        assert.equal(queries, statements);
        return reply;
      } finally {
        stop();
      }
    }

    const mismatch = await reset("new-password", "different", 1);
    assert.equal(mismatch.status, 400);
    assert.equal(mismatch.json().error.kind, "PasswordMismatch");
    assert.equal((await reset("new-password", "new-password", 1, ["1", "2"])).status, 400);
    assert.deepEqual((await audit(db, models)).map((row) => row[1]), ["login", "login"]);
    const weak = await reset("password", "password", 3);
    assert.equal(weak.status, 400);
    assert.equal(weak.json().error.kind, "WeakPassword");
    assert.deepEqual(weak.json().error.errors.map((e: Loose) => e.code), ["password_too_common"]);
    assert.equal((await getActor()).password, oldHash);
    const password = "new-private-password";
    const done = await reset(password, password, 4);
    assert.equal(done.status, 200, done.text);
    assert.ok(!done.text.includes(password));
    assert.ok(done.cookie("__Secure-monorm-session"));
    assert.equal((await actor.get(ME)).status, 200);
    assert.equal((await other.get(ME)).status, 401);
    assert.ok((await getActor()).checkPassword(password));
    const rows: Loose[] = await entry.objects.using(db).filter(entry.action.in(["password_set", "password_set_failed"])).orderBy(entry.id);
    assert.deepEqual(rows.map((r) => [r.action, r.userKey, r.resource, r.recordKey]), [
      ["password_set_failed", "1", "User", "1"], ["password_set", "1", "User", "1"],
    ]);
    assert.deepEqual(rows[0].changes.codes, weak.json().error.errors.map((e: Loose) => e.code));
    assert.ok(rows.every((r) => Object.keys(r.changes).every((name) => ["ip", "request_id", "codes"].includes(name))));
    assert.ok(!JSON.stringify(rows.map((r) => r.changes)).includes(password));
  }));

  test(`set password requires change and row scope and guards superusers (${provider})`, () => world(provider, async ({ site, db, models }) => {
    const user = models["User"];
    const entry = models["AdminLogEntry"];
    site.register(user, {
      setPassword: true,
      queryset: user.objects.filter(user.id.lt(5)).prepare(),
      can: { change: user.id.ne(3) },
    });
    await site.syncPermissions(db);
    const original = makePassword("pw");
    const users = user.objects.using(db);
    await users.insertMany(["staff", "target", "outside"].map((name) => ({ username: name, email: `${name}@example.com`, password: original })));
    const perms = models["Permission"];
    const granted: Loose[] = await perms.objects.using(db).filter(perms.codename.in(["access", "read_user"]));
    const [role] = await models["Role"].objects.using(db).insertMany([{ name: "staff" }]).returning();
    const rolePermissions = models["RolePermission"].objects.using(db);
    await rolePermissions.insertMany(granted.map((p) => ({ roleId: role.id, permissionId: p.id })));
    await models["UserRole"].objects.using(db).insertMany([{ userId: 3, roleId: role.id }]);

    const staff = site.testClient(db, { secret: SECRET });
    const target = site.testClient(db, { secret: SECRET });
    assert.equal((await staff.login("staff", "pw")).status, 200);
    assert.equal((await target.login("target", "pw")).status, 200);
    const password = "staff-issued-password";
    async function reset(key: number, status: number, statements: number) {
      const get = () => users.filter(user.id.eq(key)).first();
      const before = (await get()).password;
      let queries = 0;
      const stop = db.onQuery(() => { queries++; });
      try {
        const reply = await staff.post("/admin/api/actions/User/set_password", {
          json: { keys: [String(key)], inputs: { new_password: password, new_password_again: password } },
        });
        assert.equal(reply.status, status, reply.text);
        assert.equal(queries, statements);
      } finally {
        stop();
      }
      const stored = await get();
      if (status === 200) assert.ok(stored.checkPassword(password));
      else assert.equal(stored.password, before);
    }

    await reset(4, 403, 1);
    const change = await perms.objects.using(db).filter(perms.codename.eq("manage_user")).first();
    await rolePermissions.insertMany([{ roleId: role.id, permissionId: change.id }]);
    await reset(5, 404, 1);
    await reset(3, 403, 1);
    await reset(1, 403, 2);
    await reset(4, 200, 4);
    assert.equal((await target.get(ME)).status, 401);
    await reset(2, 200, 4);
    const rows: Loose[] = await entry.objects.using(db).orderBy(entry.id);
    assert.deepEqual(rows.filter((r) => r.action !== "login").map((r) => [r.action, r.userKey, r.recordKey]), [
      ["permission_denied", "3", ""],
      ["action_denied", "3", "3"],
      ["password_set_failed", "3", "1"],
      ["password_set", "3", "4"],
      ["password_set", "3", "2"],
    ]);
  }));

  test(`set password applies the action rule and counts the transaction queries (${provider})`, () => world(provider, async ({ site, db, models }) => {
    const user = models["User"];
    const entry = models["AdminLogEntry"];
    const reset = action(async () => { throw new Error("Rust runs the built-in action"); }, {
      name: "set_password", placement: "detail", can: user.id.eq(1),
    });
    site.register(user, { setPassword: true, actions: [reset] });
    const client = site.testClient(db, { user: { key: 1, is_superuser: true } });
    const password = "bounded-query-password";
    const run = (key: string) => client.post("/admin/api/actions/User/set_password", {
      json: { keys: [key], inputs: { new_password: password, new_password_again: password } },
    });
    const preamble = (await client.get("/admin/api/resources")).queries;
    const refused = await run("2");
    assert.deepEqual([refused.status, refused.queries], [403, preamble + 2], JSON.stringify(refused.sql));
    const done = await run("1");
    assert.deepEqual([done.status, done.queries], [200, preamble + 6], JSON.stringify(done.sql));
    const users = user.objects.using(db);
    assert.ok((await users.filter(user.id.eq(1)).first()).checkPassword(password));
    assert.ok((await users.filter(user.id.eq(2)).first()).checkPassword("pw"));
    const rows: Loose[] = await entry.objects.using(db);
    assert.deepEqual(rows.map((r) => [r.action, r.recordKey]), [["action_denied", "2"], ["password_set", "1"]]);
  }));
}
