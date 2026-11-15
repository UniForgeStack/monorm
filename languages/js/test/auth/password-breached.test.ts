/** The `breached` rule against a local stub of the range API: no real network. The Python twin is `test_password_breached.py`. */

import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { createServer, type IncomingHttpHeaders, type Server } from "node:http";
import { createServer as createNet } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test, type TestContext } from "node:test";

import { loadRules, passwordHelpTexts, rules, validatePassword, type PasswordRule } from "../../src/auth/index.js";

// SHA-1 of "password" is 5BAA61E4C9B93F3F0682250B6CF8331B7EE68FD8.
const PREFIX = "5BAA6";
const SUFFIX = "1E4C9B93F3F0682250B6CF8331B7EE68FD8";
const RANGE = ["0018A45C4D1DEF81644B54AB7F969B88D65:1", `${SUFFIX}:3730471`, "00D4F6E8FA6EECAD2A3AA415EEC418D38EC:0"].join("\r\n") + "\r\n";

const START = process.cwd();
const DIRS: string[] = [];
const SERVERS: Server[] = [];
after(() => {
  process.chdir(START);
  for (const dir of DIRS) rmSync(dir, { recursive: true, force: true });
  for (const server of SERVERS) {
    server.closeAllConnections();
    server.close();
  }
});

interface Stub {
  url: string;
  requests: { path: string; headers: IncomingHttpHeaders }[];
}

/** A range server: it records each request and answers with `status` and `body`, after `delay` milliseconds. */
async function serve(options: { body?: string; status?: number; delay?: number } = {}): Promise<Stub> {
  const { body = RANGE, status = 200, delay = 0 } = options;
  const requests: Stub["requests"] = [];
  const server = createServer((req, res) => {
    requests.push({ path: req.url ?? "", headers: req.headers });
    setTimeout(() => res.writeHead(status, { "Content-Length": Buffer.byteLength(body) }).end(body), delay);
  });
  SERVERS.push(server);
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  assert(address !== null && typeof address === "object");
  return { url: `http://127.0.0.1:${address.port}`, requests };
}

/** An address where nothing listens. */
async function closedPortUrl(): Promise<string> {
  const server = createNet();
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  assert(address !== null && typeof address === "object");
  await new Promise((resolve) => server.close(resolve));
  return `http://127.0.0.1:${address.port}`;
}

async function codes(rule: PasswordRule, password = "password"): Promise<string[]> {
  return (await validatePassword(password, null, { rules: [rule] })).map((e) => e.code);
}

function project(settings: string): string {
  const dir = mkdtempSync(join(tmpdir(), "monorm-breached-"));
  DIRS.push(dir);
  writeFileSync(join(dir, "monorm.toml"), settings);
  process.chdir(dir);
  return dir;
}

test("a breached password is refused and the host gets only five hash characters", async () => {
  const stub = await serve();
  const errors = await validatePassword("password", null, { rules: [rules.breached({ host: stub.url })] });
  assert.deepEqual(errors.map((e) => [e.code, e.params]), [["password_breached", { count: 3730471 }]]);
  assert.equal(stub.requests.length, 1);
  const [{ path, headers }] = stub.requests as [Stub["requests"][number]];
  assert.equal(path, `/range/${PREFIX}`);
  assert.equal(headers["add-padding"], "true");
  assert(!path.includes(SUFFIX) && !path.includes("password"));
});

test("a password that is not in the answer passes and a padding line does not count", async () => {
  const stub = await serve({ body: `00D4F6E8FA6EECAD2A3AA415EEC418D38EC:3\r\n${SUFFIX}:0\r\n` });
  assert.deepEqual(await codes(rules.breached({ host: stub.url })), []);
  assert.deepEqual(await codes(rules.breached({ host: stub.url }), "another long password"), []);
});

test("the count must be above the threshold and a host slash is ignored", async () => {
  const stub = await serve();
  assert.deepEqual(await codes(rules.breached({ host: `${stub.url}/`, threshold: 3730470 })), ["password_breached"]);
  assert.deepEqual(await codes(rules.breached({ host: stub.url, threshold: 3730471 })), []);
  assert.deepEqual([...new Set(stub.requests.map((r) => r.path))], [`/range/${PREFIX}`]);
});

test("fail open accepts on a timeout a network error and a bad answer and logs a warning", async (t: TestContext) => {
  const secret = "Tr0ub4dor&3";
  const slow = await serve({ delay: 5000 });
  const broken = await serve({ status: 500, body: "oops" });
  const garbled = await serve({ body: `${createHash("sha1").update(secret).digest("hex").toUpperCase().slice(5)}:many\r\n` });
  const warn = t.mock.method(console, "warn", () => {});
  for (const url of [slow.url, await closedPortUrl(), broken.url, garbled.url]) {
    warn.mock.resetCalls();
    assert.deepEqual(await codes(rules.breached({ host: url, timeout: 0.3 }), secret), []);
    assert.equal(warn.mock.callCount(), 1);
    const text = String(warn.mock.calls[0]?.arguments[0]);
    assert(!text.includes(secret) && !text.includes(SUFFIX) && !text.includes(PREFIX));
  }
});

test("fail closed refuses on a timeout a network error and a bad answer", async (t: TestContext) => {
  const slow = await serve({ delay: 5000 });
  const broken = await serve({ status: 500, body: "oops" });
  t.mock.method(console, "warn", () => {});
  for (const url of [slow.url, await closedPortUrl(), broken.url]) {
    assert.deepEqual(await codes(rules.breached({ host: url, timeout: 0.3, fail: "closed" })), ["password_check_unavailable"]);
  }
});

test("a count line that is not plain digits is a check that did not finish", async (t: TestContext) => {
  t.mock.method(console, "warn", () => {});
  for (const count of ["-5", "1_000", "\u0663\u0664", "many", ""]) {
    const stub = await serve({ body: `${SUFFIX}:${count}\r\n` });
    assert.deepEqual(await codes(rules.breached({ host: stub.url, fail: "closed" })), ["password_check_unavailable"]);
    assert.deepEqual(await codes(rules.breached({ host: stub.url })), []);
  }
});

test("fail closed still accepts a good answer", async () => {
  const stub = await serve({ body: "00D4F6E8FA6EECAD2A3AA415EEC418D38EC:3\r\n" });
  assert.deepEqual(await codes(rules.breached({ host: stub.url, fail: "closed" })), []);
});

test("breached is not in the default rules and the settings build it", async () => {
  project("");
  assert.deepEqual(await passwordHelpTexts(), [
    "Your password must contain at least 8 characters.",
    "Your password can't be a commonly used password.",
    "Your password can't be entirely numeric.",
    "Your password can't be too similar to your other personal information.",
  ]);
  const stub = await serve();
  project(`[auth]\npassword_rules = [{rule = 'min_length', min = 12}, {rule = 'breached', host = '${stub.url}', fail = 'closed'}]\n`);
  assert.deepEqual((await validatePassword("password")).map((e) => e.code), ["password_too_short", "password_breached"]);
  assert.equal((await passwordHelpTexts())[1], "Your password can't be one that appeared in a known data breach.");
});

test("a wrong breached option is an error in the settings and in code", async () => {
  project("[auth]\npassword_rules = [{rule = 'breached', fail = 'shut'}]\n");
  await assert.rejects(loadRules(), /`auth\.password_rules\[0\]\.fail` must be "open" or "closed"/);
  assert.throws(() => rules.breached({ timeout: 0 }), /timeout/);
  assert.throws(() => rules.breached({ host: "pwned.local" }), /host/);
});
