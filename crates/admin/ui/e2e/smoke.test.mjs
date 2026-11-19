// Browser smoke test of the admin UI. It starts a real `python -m monorm admin` process on SQLite (see harness.mjs),
// signs in, opens a list and a detail page, and fails on any console error, CSP violation or failed request.
// Needs: the `monorm` wheel in the active Python (`maturin develop`), `npm install` and `npm run install-browser` here.
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { cpSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { after, before, test } from "node:test";

import { chromium } from "playwright";

import { CLIENT_SECRET, startIdp, CLIENT_ID } from "./fake-idp.mjs";
import { HELD_ROLES, PYTHON, ROOT, SECRET, ZONE, startProject } from "./harness.mjs";

let dir, server, base, browser;

before(async () => {
  ({ dir, server, base } = await startProject());
  browser = await chromium.launch();
}, { timeout: 120_000 });

after(async () => {
  await browser?.close();
  if (server && server.exitCode === null) {
    server.removeAllListeners("exit");
    server.kill("SIGTERM");
  }
  if (dir) rmSync(dir, { recursive: true, force: true });
});

test("sign in, open a list and a detail page", async () => {
  const context = await browser.newContext({ timezoneId: ZONE, locale: "en-US" });
  const page = await context.newPage();
  const problems = [];
  page.on("console", (m) => m.type() === "error" && !m.text().includes("status of 401") && problems.push(`console: ${m.text()}`));
  page.on("pageerror", (e) => problems.push(`page error: ${e.message}`));
  page.on("requestfailed", (r) => problems.push(`request failed: ${r.url()} ${r.failure()?.errorText}`));
  page.on("response", (r) => r.status() >= 400 && r.status() !== 401 && problems.push(`HTTP ${r.status()}: ${r.url()}`));

  await page.goto(`${base}/admin/`);
  await page.waitForURL(`${base}/admin/-/login`);
  await page.getByLabel("Username").fill("ann");
  await page.getByLabel("Password").fill("wrong");
  await page.getByRole("button", { name: "Sign in" }).click();
  await page.getByRole("alert").waitFor();
  await page.getByLabel("Password").fill("pw-of-ann");
  await page.getByRole("button", { name: "Sign in" }).click();

  const nav = page.getByRole("navigation", { name: "Resources" });
  await nav.getByRole("link", { name: "Book", exact: true }).click();
  await page.waitForURL(`${base}/admin/Book`);
  await page.getByRole("link", { name: "1", exact: true }).waitFor();
  assert.ok(await page.getByText("6:34:05 AM").first().isVisible(), "the time shows in the browser time zone");

  await page.getByRole("link", { name: "1", exact: true }).click();
  await page.waitForURL(`${base}/admin/Book/1`);
  await page.getByRole("heading", { name: /Book/ }).waitFor();
  await page.getByText("First").first().waitFor();
  await page.getByRole("link", { name: "Author 1" }).click();
  await page.waitForURL(`${base}/admin/Author/1`);
  // A reload aborts a request still in flight, which the failed-request check would report.
  await page.getByRole("heading", { name: /Author/ }).waitFor();

  await page.reload();
  await page.getByRole("heading", { name: /Author/ }).waitFor();
  assert.deepEqual(problems, []);
  await context.close();
});

test("the shell: home, sidebar filter, theme toggle, keyboard focus, kit import and sign out", async () => {
  const { context, page, problems, idle } = await signedIn();
  // The home page: a card for each group, and the card of the last audit rows (it can be empty here: the next tests make rows).
  await page.getByRole("heading", { name: "Admin", exact: true }).waitFor();
  await page.getByRole("heading", { name: "Users and access" }).waitFor();
  await page.getByRole("heading", { name: "Recent activity" }).waitFor();
  await idle();

  // The sidebar: a group collapses, and the filter finds a resource and opens its group.
  const nav = page.getByRole("navigation", { name: "Resources" });
  const group = nav.getByRole("button", { name: /Users and access/ });
  assert.equal(await group.getAttribute("aria-expanded"), "true");
  await nav.getByRole("link", { name: "Role", exact: true }).waitFor();
  await group.click();
  assert.equal(await group.getAttribute("aria-expanded"), "false");
  assert.equal(await nav.getByRole("link", { name: "Role", exact: true }).count(), 0, "a shut group hides its links");
  await group.click();
  await page.getByRole("searchbox", { name: "Filter the resources" }).fill("rev");
  await nav.getByRole("link", { name: "Reviewer", exact: true }).waitFor();
  assert.equal(await nav.getByRole("link", { name: "Book", exact: true }).count(), 0, "the filter hides the other resources");
  await page.getByRole("searchbox", { name: "Filter the resources" }).fill("");
  assert.equal(await group.locator("svg").count(), 2, "the built-in group has a chevron and its own icon");

  // The group of the open page can collapse, and a hidden sidebar keeps its filter and its collapsed groups.
  await nav.getByRole("link", { name: "Role", exact: true }).click();
  await page.waitForURL(/\/admin\/Role$/);
  assert.equal(await group.getAttribute("aria-expanded"), "true", "the group of the open page is shown when the page opens");
  await group.click();
  assert.equal(await group.getAttribute("aria-expanded"), "false", "the group of the open page collapses");
  const filter = page.getByRole("searchbox", { name: "Filter the resources" });
  await filter.fill("zzz");
  await page.getByRole("button", { name: "Hide the sidebar" }).click();
  await page.getByRole("button", { name: "Show the sidebar" }).click();
  assert.equal(await filter.inputValue(), "zzz", "the filter survives a hide");
  await filter.fill("");
  assert.equal(await group.getAttribute("aria-expanded"), "false", "the collapsed group survives a hide");
  await group.click();
  assert.equal(await group.getAttribute("aria-expanded"), "true");
  await nav.getByRole("link", { name: "Home", exact: true }).click();
  await page.waitForURL(/\/admin\/?$/);

  // The sidebar toggle is a button with a state, and the choice survives a reload.
  const toggle = page.getByRole("button", { name: "Hide the sidebar" });
  assert.equal(await toggle.getAttribute("aria-expanded"), "true");
  await toggle.click();
  await page.getByRole("button", { name: "Show the sidebar" }).waitFor();
  await page.reload();
  await page.getByRole("button", { name: "Show the sidebar" }).waitFor();
  await page.getByRole("button", { name: "Show the sidebar" }).click();

  // Keyboard: Tab reaches the sidebar toggle, and the focus ring is visible.
  await page.locator("body").click({ position: { x: 1, y: 1 } });
  let ring = null;
  for (let i = 0; i < 12 && !ring; i++) {
    await page.keyboard.press("Tab");
    ring = await page.evaluate(() => {
      const el = document.activeElement;
      if (!el || el.getAttribute("aria-label") !== "Hide the sidebar") return null;
      const css = getComputedStyle(el);
      return { style: css.outlineStyle, width: css.outlineWidth };
    });
  }
  assert.ok(ring && ring.style !== "none" && ring.width !== "0px", `the keyboard focus shows a ring: ${JSON.stringify(ring)}`);

  // The kit for packages: the import map names `monorm-admin/ui`, and the shim has every name of the kit.
  const names = await page.evaluate(async () => {
    const shim = await import("monorm-admin/ui");
    return { shim: Object.keys(shim).sort(), kit: Object.keys(globalThis.__monormAdmin.ui).sort() };
  });
  assert.deepEqual(names.shim, names.kit);
  for (const name of ["Button", "Input", "Select", "Dialog", "Badge", "Card", "Tabs", "Table", "toast"]) assert.ok(names.kit.includes(name), `${name} is in the kit`);

  // The theme: the user menu switches to dark, the page keeps it after a reload, and System follows the browser.
  const menu = page.getByRole("button", { name: "User menu" });
  await menu.click();
  await page.getByRole("menuitemradio", { name: "Dark" }).click();
  await page.waitForFunction(() => document.documentElement.classList.contains("dark"));
  assert.equal(await page.evaluate(() => localStorage.getItem("monorm-admin-theme")), "dark");
  await page.reload();
  await page.getByRole("heading", { name: "Admin", exact: true }).waitFor();
  assert.ok(await page.evaluate(() => document.documentElement.classList.contains("dark")), "dark after a reload");
  await menu.click();
  await page.getByRole("menuitemradio", { name: "System" }).click();
  await page.emulateMedia({ colorScheme: "light" });
  await page.waitForFunction(() => !document.documentElement.classList.contains("dark"));
  await page.emulateMedia({ colorScheme: "dark" });
  await page.waitForFunction(() => document.documentElement.classList.contains("dark"));
  await page.emulateMedia({ colorScheme: "light" });

  // Sign out leaves the session.
  await menu.click();
  await page.getByRole("menuitem", { name: "Sign out" }).click();
  await page.waitForURL(`${base}/admin/-/login`);
  await idle();
  assert.deepEqual(problems, []);
  await context.close();
});

test("the site theme reaches the page: brand colour, accent surface and logo", async () => {
  const { context, page } = await signedIn();
  // The metadata route is stubbed with a theme; the Rust side of the theme has its own tests.
  await page.route(/\/api\/resources$/, async (route) => {
    const response = await route.fetch();
    const body = await response.json();
    body.brand = { ...body.brand, name: "Acme", logo: "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='8' height='8'/%3E", colors: { primary: "#7c3aed", accent: "#0b7a5b" } };
    await route.fulfill({ response, json: body });
  });
  for (const scheme of ["light", "dark"]) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.goto(`${base}/admin/`);
    await page.getByRole("heading", { name: "Acme", exact: true }).waitFor();
    const seen = await page.evaluate(() => {
      const css = getComputedStyle(document.documentElement);
      return { brand: css.getPropertyValue("--brand").trim(), accent: css.getPropertyValue("--accent").trim(), foreground: css.getPropertyValue("--accent-foreground").trim(), logo: document.querySelector("header img")?.getAttribute("src") ?? null };
    });
    assert.equal(seen.brand, "#7c3aed");
    assert.ok(seen.logo?.startsWith("data:image/svg+xml"), "the logo is in the header");
    assert.notEqual(seen.accent, scheme === "light" ? "#e9eef5" : "#1f2a4d", `the accent colour changes the accent token (${scheme})`);
    const lum = (hex) => [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255).map((v) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4)).reduce((a, v, i) => a + v * [0.2126, 0.7152, 0.0722][i], 0);
    const [x, y] = [lum(seen.accent), lum(seen.foreground)];
    assert.ok((Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05) >= 4.5, `text on the accent surface reads (${scheme}): ${seen.accent} on ${seen.foreground}`);
  }
  await context.close();
});

// WCAG AA: 4.5:1 for text and 3:1 for the border of a control, in both themes, from the resolved colour tokens.
for (const scheme of ["light", "dark"]) {
  test(`the colour tokens meet WCAG AA (${scheme})`, async () => {
    const context = await browser.newContext({ colorScheme: scheme });
    const page = await context.newPage();
    await page.goto(`${base}/admin/-/login`);
    await page.getByRole("button", { name: "Sign in" }).waitFor();
    const tokens = await page.evaluate(() => {
      const css = getComputedStyle(document.documentElement);
      return Object.fromEntries(["background", "foreground", "card", "card-foreground", "primary", "primary-foreground", "link", "secondary", "secondary-foreground", "muted", "muted-foreground", "accent", "accent-foreground", "destructive", "destructive-foreground", "success", "warning", "input"].map((n) => [n, css.getPropertyValue(`--${n}`).trim()]));
    });
    const lum = (token) => {
      assert.match(token, /^#([0-9a-f]{3}|[0-9a-f]{6})$/i, `a token is a hex colour: ${token}`);
      const hex = token.length === 4 ? `#${[...token.slice(1)].map((c) => c + c).join("")}` : token;
      const [r, g, b] = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255).map((v) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4));
      return 0.2126 * r + 0.7152 * g + 0.0722 * b;
    };
    const ratio = (a, b) => {
      const [x, y] = [lum(a), lum(b)];
      return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
    };
    const text = [["foreground", "background"], ["card-foreground", "card"], ["muted-foreground", "background"], ["muted-foreground", "card"], ["muted-foreground", "muted"], ["primary-foreground", "primary"], ["link", "background"], ["link", "card"], ["secondary-foreground", "secondary"], ["accent-foreground", "accent"], ["destructive-foreground", "destructive"], ["destructive", "card"], ["success", "card"], ["warning", "card"]];
    const low = text.map(([f, b]) => [f, b, ratio(tokens[f], tokens[b])]).filter(([, , r]) => r < 4.5);
    assert.deepEqual(low, [], "text pairs under 4.5:1");
    assert.ok(ratio(tokens.input, tokens.card) >= 3, `the control border is at least 3:1: ${ratio(tokens.input, tokens.card)}`);
    await context.close();
  });
}

test("sort a column and search a list", async () => {
  const context = await browser.newContext({ timezoneId: ZONE, locale: "en-US" });
  const page = await context.newPage();
  const problems = [];
  page.on("console", (m) => m.type() === "error" && !m.text().includes("status of 401") && problems.push(`console: ${m.text()}`));
  page.on("pageerror", (e) => problems.push(`page error: ${e.message}`));
  page.on("requestfailed", (r) => problems.push(`request failed: ${r.url()} ${r.failure()?.errorText}`));
  page.on("response", (r) => r.status() >= 400 && r.status() !== 401 && problems.push(`HTTP ${r.status()}: ${r.url()}`));

  await page.goto(`${base}/admin/-/login`);
  await page.getByLabel("Username").fill("ann");
  await page.getByLabel("Password").fill("pw-of-ann");
  await page.getByRole("button", { name: "Sign in" }).click();
  await page.getByRole("navigation", { name: "Resources" }).getByRole("link", { name: "Book", exact: true }).click();
  await page.waitForURL(`${base}/admin/Book`);
  const titles = async () => (await page.locator("tbody tr td:nth-child(3)").allTextContents()).map((t) => t.trim());
  await page.getByText("2 records").waitFor();
  // The old rows stay on screen until the new ones arrive, so each step polls for its rows.
  const rowsAre = async (expected) => {
    for (let i = 0; i < 100; i++) {
      if (JSON.stringify(await titles()) === JSON.stringify(expected)) return;
      await page.waitForTimeout(100);
    }
    assert.deepEqual(await titles(), expected);
  };
  await rowsAre(["First", "Second"]);

  // A first click sorts ascending, a second one descending.
  const header = page.getByRole("columnheader", { name: /Title/ });
  await header.getByRole("button").click();
  await page.locator('th[aria-sort="ascending"]').waitFor();
  await rowsAre(["First", "Second"]);
  await header.getByRole("button").click();
  await page.locator('th[aria-sort="descending"]').waitFor();
  await rowsAre(["Second", "First"]);

  await page.getByRole("searchbox", { name: "Search" }).fill("sec");
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await rowsAre(["Second"]);
  await page.getByText("1 record", { exact: true }).waitFor();
  // The count has arrived, so closing the context aborts no request that is still in flight.
  await page.getByRole("heading", { name: /Book/ }).waitFor();
  assert.deepEqual(problems, []);
  await context.close();
});

test("filter a list, keep the view in the URL, and open a tab", async () => {
  const context = await browser.newContext({ timezoneId: ZONE, locale: "en-US" });
  const page = await context.newPage();
  const problems = [];
  const idle = trackRequests(page);
  page.on("console", (m) => m.type() === "error" && !m.text().includes("status of 401") && problems.push(`console: ${m.text()}`));
  page.on("pageerror", (e) => problems.push(`page error: ${e.message}`));
  page.on("requestfailed", (r) => problems.push(`request failed: ${r.url()} ${r.failure()?.errorText}`));
  page.on("response", (r) => r.status() >= 400 && r.status() !== 401 && problems.push(`HTTP ${r.status()}: ${r.url()}`));

  await page.goto(`${base}/admin/-/login`);
  await page.getByLabel("Username").fill("ann");
  await page.getByLabel("Password").fill("pw-of-ann");
  await page.getByRole("button", { name: "Sign in" }).click();
  await page.getByRole("navigation", { name: "Resources" }).getByRole("link", { name: "Book", exact: true }).click();
  await page.waitForURL(`${base}/admin/Book`);
  const titles = async () => (await page.locator("tbody tr td:nth-child(3)").allTextContents()).map((t) => t.trim());
  const rowsAre = async (expected) => {
    for (let i = 0; i < 100; i++) {
      if (JSON.stringify(await titles()) === JSON.stringify(expected)) return;
      await page.waitForTimeout(100);
    }
    assert.deepEqual(await titles(), expected);
  };
  await page.getByText("2 records").waitFor();
  await rowsAre(["First", "Second"]);

  // A relation filter in the popover: pick the author from the options. A chip in the bar carries the author's name.
  const filtersButton = page.getByRole("button", { name: /^Filters/ });
  await filtersButton.click();
  await page.getByText("Choose Author").click();
  await page.getByRole("button", { name: "Ada", exact: true }).click();
  await page.waitForURL(`${base}/admin/Book?f.author=1`);
  await rowsAre(["First"]);
  await page.keyboard.press("Escape");
  const chips = page.getByRole("list", { name: "Active filters" });
  await chips.getByText("Author: Ada").waitFor();
  await chips.getByRole("button", { name: /^Remove filter Author/ }).click();
  await page.waitForURL(`${base}/admin/Book`);
  await rowsAre(["First", "Second"]);
  await chips.waitFor({ state: "detached" });

  // One filter: the books with no author. The view is in the URL, and it comes back after a reload.
  await filtersButton.click();
  await page.getByLabel("Author: empty").selectOption("empty");
  await page.waitForURL(`${base}/admin/Book?f.author%3Aempty=1`);
  await rowsAre(["Second"]);
  await page.getByText("1 record", { exact: true }).waitFor();
  await chips.getByText("Author is empty").waitFor();
  // The filter counts load after the rows, and a reload aborts a request still in flight.
  await idle();
  await page.reload();
  await rowsAre(["Second"]);
  await page.getByText("1 record", { exact: true }).waitFor();
  await chips.getByText("Author is empty").waitFor();
  await page.getByRole("button", { name: /^Filters/ }).click();
  assert.equal(await page.getByLabel("Author: empty").inputValue(), "empty");
  await page.keyboard.press("Escape");
  // Leaving the list cancels a request still in flight, which the failed-request check would report.
  await idle();

  // The detail page links back to the list as the user left it, and the back button does too.
  await page.getByRole("link", { name: "2", exact: true }).click();
  await page.waitForURL(`${base}/admin/Book/2`);
  await page.getByRole("heading", { name: /Book/ }).waitFor();
  await page.getByText("Second").first().waitFor();
  await page.getByRole("link", { name: /← Book/ }).click();
  await page.waitForURL(`${base}/admin/Book?f.author%3Aempty=1`);
  await rowsAre(["Second"]);
  await page.getByText("1 record", { exact: true }).waitFor();

  // One tab: the books of the author. A full load aborts a request still in flight, so the list settles first.
  await idle();
  await page.goto(`${base}/admin/Author/1`);
  await page.getByRole("heading", { name: /Author/ }).waitFor();
  await page.getByRole("tab", { name: "Books" }).click();
  await page.waitForURL(`${base}/admin/Author/1?tab=books`);
  await rowsAre(["First"]);
  await page.getByText("1 record", { exact: true }).waitFor();
  assert.deepEqual(problems, []);
  await context.close();
});

test("edit two cells on two rows of a list and save them in one request", async () => {
  const { context, page, problems, idle } = await signedIn();
  await page.goto(`${base}/admin/Shelf`);
  await page.getByLabel("Title of 1").waitFor();
  const saves = [];
  page.on("request", (r) => r.method() === "PATCH" && saves.push(r.url()));
  // Typing keeps the focus in the cell, and Tab moves to the next cell.
  await page.getByLabel("Title of 1").fill("Birch");
  await page.getByLabel("Title of 1").press("Tab");
  assert.equal(await page.evaluate(() => document.activeElement?.getAttribute("aria-label")), "Qty of 1");
  await page.getByLabel("Qty of 3").fill("30");
  await page.getByRole("status").filter({ hasText: "2 rows changed" }).waitFor();
  assert.equal(await page.getByLabel("Title of 1").getAttribute("data-changed"), "true");
  // A link to another page asks before it drops the changes; a dismissed question keeps them.
  const asked = [];
  page.once("dialog", (d) => { asked.push(d.message()); void d.dismiss(); });
  await page.getByRole("navigation", { name: "Resources" }).getByRole("link", { name: "Home" }).click();
  await idle();
  assert.equal(asked.length, 1);
  assert.match(asked[0], /^You have unsaved changes/);
  assert.equal(page.url(), `${base}/admin/Shelf`);
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.getByRole("group", { name: "Unsaved changes" }).waitFor({ state: "detached" });
  await idle();
  assert.equal(saves.length, 1);
  assert.equal(await page.getByLabel("Title of 1").inputValue(), "Birch");
  assert.equal(await page.getByLabel("Qty of 3").inputValue(), "30");
  // The stored values: a reload shows them.
  await page.reload();
  assert.equal(await page.getByLabel("Title of 1").inputValue(), "Birch");
  assert.equal(await page.getByLabel("Title of 2").inputValue(), "Pine");
  assert.equal(await page.getByLabel("Qty of 3").inputValue(), "30");
  assert.deepEqual(problems, []);
  await context.close();
});

test("a tab that loaded first still saves after another tab renews the CSRF token", async () => {
  const { context, page, problems, idle } = await signedIn();
  await page.goto(`${base}/admin/Shelf`);
  await page.getByLabel("Title of 1").waitFor();
  await idle();
  // The second tab asks `/me`, which writes a new token to the shared cookie.
  const second = await context.newPage();
  await second.goto(`${base}/admin/Shelf`);
  await second.getByLabel("Title of 1").waitFor();
  const statuses = [];
  page.on("response", (r) => r.request().method() === "PATCH" && statuses.push(r.status()));
  await page.getByLabel("Title of 1").fill("Cedar");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.getByRole("group", { name: "Unsaved changes" }).waitFor({ state: "detached" });
  await idle();
  assert.deepEqual(statuses, [200]);
  assert.deepEqual(problems, []);
  await context.close();
});

test("a refused cell shows its error on the cell, keeps every change, and saves after the fix", async () => {
  const { context, page, problems, idle } = await signedIn();
  await page.goto(`${base}/admin/Shelf`);
  await page.getByLabel("Title of 2").waitFor();
  await page.getByLabel("Title of 2").fill("A title that is too long");
  await page.getByLabel("Qty of 1").fill("11");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  const cell = page.getByLabel("Title of 2");
  await page.locator("#e-2-title-msg .msg-error").waitFor();
  assert.equal(await cell.getAttribute("aria-invalid"), "true");
  await page.getByText("Nothing was saved").waitFor();
  // Both changes stay, and nothing was written.
  assert.equal(await page.getByLabel("Qty of 1").inputValue(), "11");
  assert.equal(await page.getByRole("status").filter({ hasText: "2 rows changed" }).count(), 1);
  await cell.fill("Short");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.getByRole("group", { name: "Unsaved changes" }).waitFor({ state: "detached" });
  await idle();
  await page.reload();
  assert.equal(await page.getByLabel("Title of 2").inputValue(), "Short");
  assert.equal(await page.getByLabel("Qty of 1").inputValue(), "11");
  // The refused save is the only failed request.
  assert.deepEqual(problems.filter((m) => !m.includes("400")), []);
  assert.equal(problems.filter((m) => m.startsWith("HTTP 400")).length, 1);
  await context.close();
});

test("a cell that another user changed shows the stored value, takes it on a click, or keeps the user's value on the next save", async () => {
  const { context, page, problems, idle } = await signedIn();
  // Another browser, so the CSRF token of each stays its own.
  const second = await signedIn();
  const other = second.page;
  await page.goto(`${base}/admin/Shelf`);
  await other.goto(`${base}/admin/Shelf`);
  await page.getByLabel("Title of 2").waitFor();
  await other.getByLabel("Title of 2").waitFor();
  const save = (p) => p.getByRole("button", { name: "Save", exact: true }).click();
  const gone = (p) => p.getByRole("group", { name: "Unsaved changes" }).waitFor({ state: "detached" });
  // The other user saves first.
  await other.getByLabel("Title of 2").fill("Beech");
  await save(other);
  await gone(other);
  // This user saves the same cell from the old value: the answer is a conflict, and nothing is written.
  const cell = page.getByLabel("Title of 2");
  await cell.fill("Ash");
  await save(page);
  await page.locator("#e-2-title-msg").getByText("Changed to Beech by someone else.").waitFor();
  assert.equal(await cell.getAttribute("aria-invalid"), "true");
  assert.equal(await cell.inputValue(), "Ash");
  await page.getByText("Another user changed some rows").waitFor();
  // Taking the stored value settles the conflict: no message, no invalid mark, no change left.
  await page.getByRole("button", { name: "Use that value" }).click();
  await gone(page);
  assert.equal(await cell.inputValue(), "Beech");
  assert.equal(await cell.getAttribute("aria-invalid"), null);
  assert.equal(await page.locator("#e-2-title-msg").count(), 1);
  assert.equal(await page.locator("#e-2-title-msg .msg").count(), 0);
  assert.equal(await page.getByText("Another user changed some rows").count(), 0);
  // A second conflict: the next save keeps the value of this user.
  await other.getByLabel("Title of 2").fill("Fir");
  await save(other);
  await gone(other);
  await cell.fill("Ash");
  await save(page);
  await page.getByText("Changed to Fir by someone else.").waitFor();
  await save(page);
  await gone(page);
  await idle();
  await page.reload();
  assert.equal(await page.getByLabel("Title of 2").inputValue(), "Ash");
  // The two refused saves are the only failed requests.
  assert.deepEqual(problems.filter((m) => !m.includes("409")), []);
  assert.equal(problems.filter((m) => m.startsWith("HTTP 409")).length, 2);
  assert.deepEqual(second.problems, []);
  await second.context.close();
  await context.close();
});

// `networkidle` fires once per document load, so a client-side navigation never waits on it. This counts the requests in flight.
function trackRequests(page) {
  const open = new Set();
  page.on("request", (r) => open.add(r));
  page.on("requestfinished", (r) => open.delete(r));
  page.on("requestfailed", (r) => open.delete(r));
  return async () => {
    let quiet = 0;
    while (quiet < 3) {
      quiet = open.size === 0 ? quiet + 1 : 0;
      await page.waitForTimeout(100);
    }
  };
}

async function signedIn(timezoneId = ZONE) {
  const context = await browser.newContext({ timezoneId, locale: "en-US" });
  const page = await context.newPage();
  const problems = [];
  const idle = trackRequests(page);
  page.on("console", (m) => m.type() === "error" && !m.text().includes("status of 401") && problems.push(`console: ${m.text()}`));
  page.on("pageerror", (e) => problems.push(`page error: ${e.message}`));
  page.on("requestfailed", (r) => problems.push(`request failed: ${r.url()} ${r.failure()?.errorText}`));
  page.on("response", (r) => r.status() >= 400 && r.status() !== 401 && problems.push(`HTTP ${r.status()}: ${r.url()}`));
  await page.goto(`${base}/admin/-/login`);
  await page.getByLabel("Username").fill("ann");
  await page.getByLabel("Password").fill("pw-of-ann");
  await page.getByRole("button", { name: "Sign in" }).click();
  await page.getByRole("navigation", { name: "Resources" }).waitFor();
  await idle();
  return { context, page, problems, idle };
}

test("add a book from the Books tab of an author, then change it, then delete it", async () => {
  const { context, page, problems, idle } = await signedIn();
  await page.goto(`${base}/admin/Author/1?tab=books`);
  await page.getByRole("link", { name: "Add", exact: true }).click();
  // The author comes from the tab: the form opens with it filled in.
  await page.waitForURL(`${base}/admin/Book/-/new?author_id=1`);
  await page.getByRole("heading", { name: "Add Book" }).waitFor();
  assert.equal(await page.getByLabel("Author id").inputValue(), "1");
  await page.getByLabel("Title").fill("Third");
  await page.getByLabel("Created at").fill("2026-02-03T10:00:00");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.waitForURL(`${base}/admin/Book/3`);
  await page.getByText("Third").first().waitFor();

  await page.getByRole("link", { name: "Edit" }).click();
  await page.waitForURL(`${base}/admin/Book/3/edit`);
  await page.getByRole("heading", { name: "Change Book" }).waitFor();
  assert.equal(await page.getByLabel("Title").inputValue(), "Third");
  await page.getByLabel("Title").fill("Third, revised");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.waitForURL(`${base}/admin/Book/3`);
  await page.getByText("Third, revised").first().waitFor();

  await page.getByRole("button", { name: "Delete" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Delete it" }).click();
  await page.waitForURL(`${base}/admin/Book`);
  await page.getByText("2 records").waitFor();
  await idle();
  assert.deepEqual(problems, []);
  await context.close();
});

test("a form shows the field error of the server, and the edit of an author is saved", async () => {
  const { context, page, problems, idle } = await signedIn();
  await page.goto(`${base}/admin/Author/-/new`);
  await page.getByRole("heading", { name: "Add Author" }).waitFor();
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.getByText("This field is required.").waitFor();
  await page.getByLabel("Name").fill("Zed");
  await page.getByRole("button", { name: "Save and continue" }).click();
  await page.waitForURL(`${base}/admin/Author/2/edit`);
  await page.getByLabel("Name").fill("Zed Two");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.waitForURL(`${base}/admin/Author/2`);
  await page.getByText("Zed Two").first().waitFor();
  await page.getByRole("button", { name: "Delete" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Delete it" }).click();
  await page.waitForURL(`${base}/admin/Author`);
  await page.getByText("1 record", { exact: true }).waitFor();
  await idle();
  // The 422 of the first save is the one error that the test expects: the response and the console line of the browser.
  assert.deepEqual(problems, [`HTTP 422: ${base}/admin/api/resources/Author`, "console: Failed to load resource: the server responded with a status of 422 (Unprocessable Entity)"]);
  await context.close();
});

test("a protobuf message shows as JSON, a bad JSON is a field error, and the edit is saved", async () => {
  const { context, page, problems, idle } = await signedIn();
  await page.goto(`${base}/admin/Setting`);
  // The server sorts the keys of the JSON.
  await page.getByText('{"priority":3,"tags":["a"],"title":"hi"}').waitFor();
  await page.goto(`${base}/admin/Setting/1`);
  assert.deepEqual(JSON.parse(await page.locator("pre.json-value").innerText()), { title: "hi", priority: 3, tags: ["a"] });
  await page.goto(`${base}/admin/Setting/1/edit`);
  const config = page.getByLabel("Config");
  assert.match(await config.evaluate((e) => getComputedStyle(e).fontFamily), /mono/i, "the JSON field is monospace");
  assert.deepEqual(JSON.parse(await config.inputValue()), { title: "hi", priority: 3, tags: ["a"] });
  // The browser refuses the text that is not JSON; the server refuses JSON that is not the message.
  await config.fill("{");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.getByText("Must be valid JSON").waitFor();
  await config.fill('{"nope": 1}');
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.locator("#field-config-msg, .field-bad .msg-error").first().waitFor();
  assert.equal(await config.getAttribute("aria-invalid"), "true");
  await config.fill('{"title": "new", "priority": 4}');
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.waitForURL(`${base}/admin/Setting/1`);
  await page.locator("pre.json-value").getByText('"new"').waitFor();
  await idle();
  assert.deepEqual(problems, [`HTTP 422: ${base}/admin/api/resources/Setting/1`, "console: Failed to load resource: the server responded with a status of 422 (Unprocessable Entity)"]);
  await context.close();
});

test("pickers, a switch, a select and a JSON editor load, change, validate and save, in the browser time zone", async () => {
  const { context, page, problems, idle } = await signedIn();
  await page.goto(`${base}/admin/Event/1/edit`);
  await page.getByRole("heading", { name: "Change Event" }).waitFor();
  // 03:04:05Z is 06:34:05 in Asia/Tehran (the browser drops zero seconds from a value: 08:00:00 reads 08:00); a date and a time of day have no zone and stay as stored.
  assert.equal(await page.getByLabel("Starts").inputValue(), "2026-01-02T06:34:05");
  assert.equal(await page.getByLabel("Day").inputValue(), "2026-03-04");
  assert.equal(await page.getByLabel("At", { exact: true }).inputValue(), "09:30:15");
  assert.equal(await page.getByLabel("Day").getAttribute("type"), "date");
  assert.equal(await page.getByLabel("At", { exact: true }).getAttribute("type"), "time");
  assert.equal(await page.getByLabel("Starts").getAttribute("type"), "datetime-local");
  const live = page.getByRole("switch", { name: "Live" });
  assert.equal(await live.isChecked(), true);
  const mood = page.getByLabel("Mood");
  assert.equal(await mood.evaluate((e) => e.tagName), "SELECT");
  assert.equal(await mood.inputValue(), "tense");
  const extra = page.getByLabel("Extra");
  assert.equal(await extra.inputValue(), '{\n  "a": 1\n}');

  // A bad JSON shows a message at once; Validate says so when it is good; Format indents it.
  await extra.fill('{"a":');
  await page.getByRole("status").filter({ hasText: "Not valid JSON" }).waitFor();
  await extra.fill('{"b":[1,2]}');
  await page.getByRole("button", { name: "Validate" }).click();
  await page.getByRole("status").filter({ hasText: "Valid JSON" }).waitFor();
  await page.getByRole("button", { name: "Format" }).click();
  assert.equal(await extra.inputValue(), '{\n  "b": [\n    1,\n    2\n  ]\n}');

  await page.getByLabel("Starts").fill("2026-01-02T08:00:00");
  await page.getByLabel("Day").fill("2026-12-31");
  await page.getByLabel("At", { exact: true }).fill("23:59:58");
  await live.click();
  await mood.selectOption("calm");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.waitForURL(`${base}/admin/Event/1`);
  await idle();

  await page.goto(`${base}/admin/Event/1/edit`);
  assert.equal(await page.getByLabel("Starts").inputValue(), "2026-01-02T08:00");
  assert.equal(await page.getByLabel("Day").inputValue(), "2026-12-31");
  assert.equal(await page.getByLabel("At", { exact: true }).inputValue(), "23:59:58");
  assert.equal(await page.getByRole("switch", { name: "Live" }).isChecked(), false);
  assert.equal(await page.getByLabel("Mood").inputValue(), "calm");
  await idle();
  assert.deepEqual(problems, []);
  await context.close();
});

test("a date keeps its day and a date with a time keeps its moment in a zone west of UTC", async () => {
  const { context, page, problems, idle } = await signedIn("America/Los_Angeles");
  await page.goto(`${base}/admin/Event/1/edit`);
  await page.getByRole("heading", { name: "Change Event" }).waitFor();
  // 08:00 in Tehran is 04:30Z, which is the evening before in Los Angeles; the date column does not move.
  assert.equal(await page.getByLabel("Starts").inputValue(), "2026-01-01T20:30");
  assert.equal(await page.getByLabel("Day").inputValue(), "2026-12-31");
  await page.getByLabel("Starts").fill("2026-06-01T12:00:00");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.waitForURL(`${base}/admin/Event/1`);
  await idle();
  await page.goto(`${base}/admin/Event/1/edit`);
  assert.equal(await page.getByLabel("Starts").inputValue(), "2026-06-01T12:00");
  await idle();
  assert.deepEqual(problems, []);
  await context.close();
});

test("a section layout with a collapsed section, and a custom form", async () => {
  const { context, page, problems, idle } = await signedIn();
  await page.goto(`${base}/admin/Book/-/new`);
  await page.getByRole("heading", { name: "Add Book" }).waitFor();
  await page.getByText("Basics", { exact: true }).waitFor();
  const more = page.locator("details.section");
  assert.equal(await more.getAttribute("open"), null, "the collapsed section starts closed");
  assert.equal(await page.getByLabel("Author id").isVisible(), false);
  await more.locator("summary").click();
  await page.getByLabel("Author id").waitFor();
  assert.equal(await page.locator("fieldset.section").count(), 1);

  await idle();
  // The module of the resource replaces the form. The first save has no title, and the 422 comes back as `errors`.
  await page.goto(`${base}/admin/Note/-/new`);
  await page.getByLabel("Note text").waitFor();
  await page.getByRole("button", { name: "Save note" }).click();
  await page.getByRole("note").getByText("This field is required.").waitFor();
  await page.getByLabel("Note text").fill("Hello");
  await page.getByRole("button", { name: "Save note" }).click();
  await page.waitForURL(`${base}/admin/Note/1`);
  await page.getByText("Hello").first().waitFor();
  await idle();
  assert.deepEqual(problems, [`HTTP 422: ${base}/admin/api/resources/Note`, "console: Failed to load resource: the server responded with a status of 422 (Unprocessable Entity)"]);
  await context.close();
});

test("pick a record with the autocomplete, create one in a modal, and upload a file", async () => {
  const { context, page, problems, idle } = await signedIn();
  await page.goto(`${base}/admin/Paper/-/new`);
  await page.getByRole("heading", { name: "Add Paper" }).waitFor();
  await page.getByLabel("Title").fill("Pick");
  const reviewer = page.getByRole("combobox", { name: "Reviewer id" });
  await reviewer.click();
  const options = page.getByRole("listbox").getByRole("option");
  await options.nth(19).waitFor();
  assert.equal(await options.count(), 20, "the first page holds 20 rows");
  await page.getByRole("button", { name: "Show more" }).click();
  await options.nth(24).waitFor();
  assert.equal(await options.count(), 25, "the next page follows by keyset");
  // A search narrows the list on the server. Each step waits for its rows.
  await reviewer.fill("Rev 23");
  await page.getByRole("option", { name: "Rev 23" }).waitFor();
  await options.nth(1).waitFor({ state: "detached" });
  await page.getByRole("option", { name: "Rev 23" }).click();
  assert.equal(await reviewer.inputValue(), "Rev 23");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.waitForURL(`${base}/admin/Paper/1`);
  await page.getByText("23").first().waitFor();

  // The modal creates a Reviewer, fills the field and leaves the other fields of the form as they were.
  await idle();
  await page.goto(`${base}/admin/Paper/-/new`);
  await page.getByLabel("Title").fill("Modal");
  await page.getByRole("combobox", { name: "Reviewer id" }).click();
  await page.getByRole("button", { name: /^New Reviewer/ }).click();
  const dialog = page.getByRole("dialog", { name: "New Reviewer" });
  await dialog.getByLabel("Name").fill("Rev New");
  await dialog.getByRole("button", { name: "Save", exact: true }).click();
  await dialog.waitFor({ state: "detached" });
  assert.equal(await page.getByRole("combobox", { name: "Reviewer id" }).inputValue(), "Rev New");
  assert.equal(await page.getByLabel("Title").inputValue(), "Modal", "the form keeps its values");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.waitForURL(`${base}/admin/Paper/2`);
  await page.getByText("26").first().waitFor();

  // One upload through the multipart route, then the download as an attachment.
  await idle();
  await page.goto(`${base}/admin/Paper/2/edit`);
  await page.getByRole("heading", { name: "Change Paper" }).waitFor();
  await page.getByLabel("Cover").setInputFiles({ name: "cover.txt", mimeType: "text/plain", buffer: Buffer.from("cover bytes") });
  const link = page.getByRole("link", { name: "cover.txt" });
  await link.waitFor();
  const got = await context.request.get(new URL(await link.getAttribute("href"), base).href);
  assert.equal(got.status(), 200);
  assert.match(got.headers()["content-disposition"], /^attachment/);
  assert.equal(got.headers()["content-type"], "text/plain");
  assert.equal(await got.text(), "cover bytes");
  await idle();
  assert.deepEqual(problems, []);
  await context.close();
});

test("change an inline row and add another with the album, in one save", async () => {
  const { context, page, problems, idle } = await signedIn();
  await page.goto(`${base}/admin/Album/1/edit`);
  await page.getByRole("heading", { name: "Change Album" }).waitFor();
  const tracks = page.locator("details.inline");
  await tracks.getByText("Tracks", { exact: true }).waitFor();
  const titles = tracks.getByLabel("Title");
  assert.equal(await titles.first().inputValue(), "Intro");
  await titles.first().fill("Intro, revised");
  await tracks.getByRole("button", { name: "Add row" }).click();
  await titles.nth(1).fill("Outro");
  await tracks.getByLabel("Plays").nth(1).fill("7");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.waitForURL(`${base}/admin/Album/1`);
  // The detail page shows the stored rows.
  await page.getByRole("cell", { name: "Intro, revised" }).waitFor();
  await page.getByRole("cell", { name: "Outro" }).waitFor();
  const stored = await (await context.request.get(`${base}/admin/api/resources/Album/1`)).json();
  const rows = stored.inlines.tracks.rows.map((r) => JSON.stringify(r.values));
  assert.equal(rows.length, 2);
  assert.ok(rows[0].includes("Intro, revised") && rows[1].includes("Outro") && rows[1].includes("7"), rows.join(" | "));
  await idle();
  assert.deepEqual(problems, []);
  await context.close();
});

test("run a bulk action on the selected rows, and on all rows that match", async () => {
  const { context, page, problems, idle } = await signedIn();
  await page.goto(`${base}/admin/Ticket?limit=2`);
  await page.getByRole("cell", { name: "Ticket 1" }).waitFor();
  assert.equal(await page.getByRole("group", { name: "Bulk actions" }).count(), 0, "no bar before a selection");
  await page.getByLabel("Select row 1").check();
  await page.getByLabel("Select row 2").check();
  await page.getByText("2 selected").waitFor();
  await page.getByRole("button", { name: "Close", exact: true }).click();
  const dialog = page.getByRole("alertdialog", { name: "Confirm Close" });
  await dialog.getByText("Close the chosen tickets?").waitFor();
  await dialog.getByRole("button", { name: "Close", exact: true }).click();
  // The message shows, the selection ends and the rows reload.
  await page.getByRole("status").filter({ hasText: "Closed 2 tickets" }).waitFor();
  await page.getByRole("group", { name: "Bulk actions" }).waitFor({ state: "detached" });
  const rows = (await (await context.request.get(`${base}/admin/api/resources/Ticket`)).json()).rows.map((r) => JSON.stringify(r));
  assert.equal(rows.filter((r) => r.includes("closed")).length, 2, rows.join(" | "));
  // The whole page, then all rows that match: the count comes from the server.
  await page.getByLabel("Select all rows of this page").check();
  await page.getByRole("button", { name: /^Select all 3 matching/ }).click();
  await page.getByText("All 3 matching records selected").waitFor();
  await page.getByRole("button", { name: "Close", exact: true }).click();
  await page.getByRole("alertdialog", { name: "Confirm Close" }).getByRole("button", { name: "Close", exact: true }).click();
  await page.getByRole("status").filter({ hasText: "Closed 3 tickets" }).waitFor();
  await idle();
  assert.deepEqual(problems, []);
  await context.close();
});

test("a list shows the related row as a link, hides a column, switches density, and has an empty state and a row menu", async () => {
  const { context, page, problems, idle } = await signedIn();
  await page.goto(`${base}/admin/Book`);
  await page.getByText("2 records").waitFor();
  // The foreign key shows the label of the related row, not its id, and links to it.
  const ada = page.getByRole("link", { name: "Ada", exact: true });
  assert.match((await ada.getAttribute("href")) ?? "", /\/Author\/1$/);
  await idle();
  await ada.click();
  await page.waitForURL(`${base}/admin/Author/1`);
  await page.goBack();
  await page.getByText("2 records").waitFor();

  // A hidden column stays hidden after a reload; the first column cannot be hidden.
  const authorHeader = page.getByRole("columnheader", { name: "Author", exact: true });
  await authorHeader.waitFor();
  await page.getByRole("button", { name: "Columns" }).click();
  assert.equal(await page.getByRole("menuitemcheckbox", { name: "Id", exact: true }).getAttribute("aria-disabled"), "true");
  await page.getByRole("menuitemcheckbox", { name: "Author", exact: true }).click();
  await authorHeader.waitFor({ state: "detached" });
  await page.keyboard.press("Escape");
  await idle();
  await page.reload();
  await page.getByText("2 records").waitFor();
  assert.equal(await page.getByRole("columnheader", { name: "Author", exact: true }).count(), 0);
  await page.getByRole("button", { name: "Columns" }).click();
  await page.getByRole("menuitemcheckbox", { name: "Author", exact: true }).click();
  await page.getByRole("columnheader", { name: "Author", exact: true }).waitFor();
  await page.keyboard.press("Escape");

  // The density toggle is a pressed button, and the choice stays after a reload.
  const density = page.getByRole("button", { name: "Compact" });
  assert.equal(await density.getAttribute("aria-pressed"), "false");
  await density.click();
  assert.equal(await density.getAttribute("aria-pressed"), "true");
  await idle();
  await page.reload();
  await page.getByText("2 records").waitFor();
  assert.equal(await page.getByRole("button", { name: "Compact" }).getAttribute("aria-pressed"), "true");
  await page.getByRole("button", { name: "Compact" }).click();

  // A search with no match shows the empty state, and its button clears the search.
  await page.getByRole("searchbox", { name: "Search" }).fill("zzz-no-match");
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page.getByText("No records match").waitFor();
  await page.getByRole("button", { name: "Clear the search and filters" }).click();
  await page.getByText("2 records").waitFor();
  await idle();

  // A row action sits in the row menu.
  await page.goto(`${base}/admin/Ticket`);
  await page.getByRole("cell", { name: "Ticket 1" }).waitFor();
  await page.getByRole("button", { name: "Actions for row 1" }).click();
  await page.getByRole("menuitem", { name: "Reopen" }).click();
  await page.getByRole("status").filter({ hasText: "Reopened" }).waitFor();
  await idle();
  assert.deepEqual(problems, []);
  await context.close();
});

test("a package widget and a package detail tab load under the page policy and call a custom route", async () => {
  const { context, page, problems, idle } = await signedIn();
  await page.goto(`${base}/admin/Movie/1/edit`);
  await page.getByRole("heading", { name: "Change Movie" }).waitFor();
  // The widget comes from the package file, with the page's own React through the import map.
  const stars = page.getByRole("group", { name: "Stars" });
  await stars.waitFor();
  assert.equal(await stars.getByRole("button", { pressed: true }).count(), 4);
  await stars.getByRole("button", { name: "5 of 5", exact: true }).click();
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.waitForURL(`${base}/admin/Movie/1`);
  const rows = (await (await context.request.get(`${base}/admin/api/resources/Movie`)).json()).rows.map((r) => JSON.stringify(r));
  assert.ok(rows[0].includes("Alpha") && rows[0].includes("5") && !rows[0].includes("4"), rows.join(" | "));
  // The tab is a component of the page (no frame) and reads the route `rating/1/summary`.
  await page.getByRole("tab", { name: "Summary" }).click();
  await page.waitForURL(`${base}/admin/Movie/1?tab=rating.summary`);
  await page.getByText("Stars: 5").waitFor();
  await page.getByText("Average of 2 rated: 3.5").waitFor();
  assert.equal(await page.locator("iframe").count(), 0);
  await idle();
  assert.deepEqual(problems, []);
  await context.close();
});

test("a site at a non-default base path signs in, opens a list, a deep link and a reload, and scopes its cookies", async () => {
  // A copy of the project whose `[admin]` section sets the base path, on its own port and database file.
  const copy = `${dir}-bundler`;
  cpSync(dir, copy, { recursive: true });
  const toml = readFileSync(join(copy, "monorm.toml"), "utf8");
  assert.ok(toml.includes("[admin]"), "the generated monorm.toml has an [admin] section");
  writeFileSync(join(copy, "monorm.toml"), toml.replace("[admin]", '[admin]\nbase_path = "/bundler/admin"'));
  const env = { ...process.env, PYTHONPATH: ROOT, MONORM_DATABASE_URL: `sqlite://${copy}/app.db`, MONORM_ADMIN_SECRET: SECRET, MONORM_ADMIN_PORT: "0" };
  const other = spawn(PYTHON, ["-m", "monorm", "admin"], { cwd: copy, env, stdio: ["ignore", "ignore", "pipe"] });
  try {
    const url = await new Promise((resolveUrl, reject) => {
      let text = "";
      other.stderr.on("data", (chunk) => {
        text += chunk;
        const found = /"url":"([^"]+)"/.exec(text);
        if (found && text.includes('"listening"')) resolveUrl(found[1]);
      });
      other.on("exit", (code) => reject(new Error(`the admin ended with ${code}\n${text}`)));
    });
    const context = await browser.newContext({ timezoneId: ZONE, locale: "en-US" });
    const page = await context.newPage();
    const problems = [];
    const idle = trackRequests(page);
    page.on("console", (m) => m.type() === "error" && !m.text().includes("status of 401") && problems.push(`console: ${m.text()}`));
    page.on("pageerror", (e) => problems.push(`page error: ${e.message}`));
    page.on("requestfailed", (r) => problems.push(`request failed: ${r.url()} ${r.failure()?.errorText}`));
    page.on("response", (r) => r.status() >= 400 && r.status() !== 401 && problems.push(`HTTP ${r.status()}: ${r.url()}`));

    // The bare base path, with no slash, loads the app, which goes to the login page under the base.
    await page.goto(`${url}/bundler/admin`);
    await page.waitForURL(`${url}/bundler/admin/-/login`);
    await page.getByLabel("Username").fill("ann");
    await page.getByLabel("Password").fill("pw-of-ann");
    await page.getByRole("button", { name: "Sign in" }).click();
    await page.getByRole("navigation", { name: "Resources" }).getByRole("link", { name: "Book", exact: true }).click();
    await page.waitForURL(`${url}/bundler/admin/Book`);
    await page.getByText("2 records").waitFor();
    // A deep link and a reload load the assets from the base path. A request still in flight would end as a failed one.
    await idle();
    await page.goto(`${url}/bundler/admin/Book/1`);
    await page.getByRole("heading", { name: /Book/ }).waitFor();
    await idle();
    await page.reload();
    await page.getByText("First").first().waitFor();
    await idle();
    const cookies = await context.cookies(`${url}/bundler/admin/`);
    const session = cookies.find((c) => c.name === "__Secure-monorm-session");
    assert.ok(session && session.path === "/bundler/admin/" && session.httpOnly, JSON.stringify(cookies));
    assert.equal((await context.request.get(`${url}/admin/api/resources/Book`)).status() === 200, false, "nothing answers under the default base");
    assert.equal((await context.request.get(`${url}/healthz`)).status(), 200, "the health route stays at the root");
    assert.deepEqual(problems, []);
    await context.close();
  } finally {
    other.removeAllListeners("exit");
    other.kill("SIGTERM");
    rmSync(copy, { recursive: true, force: true });
  }
});

test("single sign-on: a user signs in at the provider, gets the roles of the claim, and a refused user sees why", async () => {
  // A copy of the project whose `[admin.oidc]` section names the fake provider; the provider listens first, so its address is known.
  const idp = await startIdp({ claims: { email: "sso@example.com", roles: ["support"] } });
  const copy = `${dir}-sso`;
  cpSync(dir, copy, { recursive: true });
  const toml = readFileSync(join(copy, "monorm.toml"), "utf8");
  writeFileSync(join(copy, "monorm.toml"), `${toml.trimEnd()}\n\n[admin.oidc]\nissuer = "${idp.issuer}"\nclient_id = "${CLIENT_ID}"\nhd = "example.com"\n`);
  const env = { ...process.env, PYTHONPATH: ROOT, MONORM_DATABASE_URL: `sqlite://${copy}/app.db`, MONORM_ADMIN_SECRET: SECRET, MONORM_ADMIN_PORT: "0", MONORM_ADMIN_OIDC_CLIENT_SECRET: CLIENT_SECRET };
  const other = spawn(PYTHON, ["-m", "monorm", "admin"], { cwd: copy, env, stdio: ["ignore", "ignore", "pipe"] });
  try {
    const url = await new Promise((resolveUrl, reject) => {
      let text = "";
      other.stderr.on("data", (chunk) => {
        text += chunk;
        const found = /"url":"([^"]+)"/.exec(text);
        if (found && text.includes('"listening"')) resolveUrl(found[1]);
      });
      other.on("exit", (code) => reject(new Error(`the admin ended with ${code}\n${text}`)));
    });
    const context = await browser.newContext({ timezoneId: ZONE, locale: "en-US" });
    const page = await context.newPage();
    // The provider sends no `hd`, so the allowed domain refuses the first login and the login page says why.
    await page.goto(`${url}/admin/`);
    await page.waitForURL(`${url}/admin/-/login`);
    await page.getByRole("link", { name: "Sign in with single sign-on" }).click();
    await page.getByText("This account is not in the allowed domain.").waitFor();
    assert.equal((await context.request.get(`${url}/admin/api/me`)).status(), 401, "no session after a refused login");
    // With the domain in the claim, the same flow signs the user in; the claim made the user and its role.
    idp.claims.hd = "example.com";
    await page.getByRole("link", { name: "Sign in with single sign-on" }).click();
    await page.waitForURL(`${url}/admin/`);
    const me = await context.request.get(`${url}/admin/api/me`);
    assert.equal(me.status(), 200);
    assert.equal((await me.json()).user.username, "sso@example.com");
    const held = spawnSync(PYTHON, ["-c", HELD_ROLES], { cwd: copy, env, encoding: "utf8" });
    assert.equal(held.status, 0, held.stderr);
    assert.equal(held.stdout.trim(), "support", "the user holds the role that the claim names");
    const cookies = await context.cookies(`${url}/admin/`);
    assert.ok(!cookies.some((c) => c.name === "__Secure-monorm-oidc"), "the flow cookie ends with the callback");
    await context.close();
  } finally {
    other.removeAllListeners("exit");
    other.kill("SIGTERM");
    await idp.close();
    rmSync(copy, { recursive: true, force: true });
  }
});

// Last: it adds an author, and the tests above count the authors.
test("the home page lists the last audit rows", async () => {
  const { context, page, idle } = await signedIn();
  await page.goto(`${base}/admin/Author/-/new`);
  await page.getByLabel("Name").fill("Home Audit Author");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.waitForURL(/\/admin\/Author\/\d+$/);
  await page.goto(`${base}/admin/`);
  const table = page.getByRole("table");
  await table.getByRole("cell", { name: "Author", exact: true }).first().waitFor();
  assert.ok((await table.getByRole("row").count()) >= 2, "a header row and at least one audit row");
  await idle();
  await context.close();
});
