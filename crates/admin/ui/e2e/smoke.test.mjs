// Browser smoke test of the admin UI. It starts a real `python -m monorm admin` process on SQLite (see harness.mjs),
// signs in, opens a list and a detail page, and fails on any console error, CSP violation or failed request.
// Needs: the `monorm` wheel in the active Python (`maturin develop`), `npm install` and `npm run install-browser` here.
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { cpSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer, request } from "node:http";
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

test("a frame tab shows the sibling admin's read-only embed with its own session", { timeout: 120_000 }, async () => {
  const front = await startProject({ frame: true });
  let sibling, proxy, context;
  try {
    sibling = await startProject({ embed: true });
    proxy = createServer((incoming, outgoing) => {
      const upstream = incoming.url.startsWith("/identity/admin") ? sibling.base : front.base;
      const forwarded = request(new URL(incoming.url, upstream), { method: incoming.method, headers: incoming.headers }, (response) => {
        outgoing.writeHead(response.statusCode, response.headers);
        response.pipe(outgoing);
      });
      forwarded.on("error", () => outgoing.destroy());
      incoming.pipe(forwarded);
    });
    await new Promise((resolve) => proxy.listen(0, "127.0.0.1", resolve));
    const origin = `http://localhost:${proxy.address().port}`;
    context = await browser.newContext();
    const page = await context.newPage();
    const problems = [];
    page.on("pageerror", (error) => problems.push(error.message));
    page.on("console", (msg) => {
      if (msg.type() === "error" && !msg.text().includes("status of 401")) problems.push(msg.text());
    });
    const signIn = async (page, path) => {
      await page.goto(`${origin}${path}/-/login`);
      await page.getByLabel("Username").fill("ann");
      await page.getByLabel("Password").fill("pw-of-ann");
      await page.getByRole("button", { name: "Sign in" }).click();
      await page.getByRole("navigation", { name: "Resources" }).waitFor();
    };
    await signIn(page, "/admin");
    await page.goto(`${origin}/admin/Book/1`);
    const tab = page.getByRole("tab", { name: "Identity health" });
    await tab.waitFor();
    assert.equal(await page.locator("iframe").count(), 0);
    await tab.click();
    const iframe = page.locator('iframe[title="Identity health"]');
    assert.equal(await iframe.getAttribute("sandbox"), "allow-scripts allow-same-origin");
    assert.equal(await iframe.getAttribute("referrerpolicy"), "same-origin");
    assert.equal(await iframe.evaluate((frame) => frame.style.height), "65vh");
    const view = iframe.contentFrame();
    const signInLink = view.getByRole("link", { name: "Sign in to Identity" });
    await signInLink.waitFor();
    assert.equal(await signInLink.getAttribute("target"), "_blank");
    assert.equal(await view.locator("form").count(), 0);

    const login = await context.newPage();
    await signIn(login, "/identity/admin");
    await login.close();
    await page.reload();
    await view.getByRole("heading", { name: "Book health" }).waitFor();
    assert.equal(await view.getByText("First", { exact: true }).count(), 1);
    assert.equal(await view.getByRole("navigation").count(), 0);
    assert.equal(await view.getByRole("button").count(), 0);
    assert.equal(await view.locator("form").count(), 0);
    const fallback = page.getByRole("link", { name: "Open in new tab" });
    assert.equal(await fallback.getAttribute("href"), "/identity/admin/-/embed/book_health?id=1");
    assert.equal(await fallback.getAttribute("target"), "_blank");
    assert.deepEqual(problems, []);
  } finally {
    await context?.close();
    proxy?.closeAllConnections();
    await new Promise((resolve) => proxy ? proxy.close(resolve) : resolve());
    for (const app of [front, sibling].filter(Boolean)) {
      app.server.kill("SIGTERM");
      rmSync(app.dir, { recursive: true, force: true });
    }
  }
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
  for (let i = 0; i < 80 && !ring; i++) {
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
  await page.route(/\/api\/site$/, async (route) => {
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
      return { brand: css.getPropertyValue("--brand").trim(), accent: css.getPropertyValue("--accent").trim(), foreground: css.getPropertyValue("--accent-foreground").trim(), logo: document.querySelector("#sidebar img")?.getAttribute("src") ?? null };
    });
    assert.equal(seen.brand, "#7c3aed");
    assert.ok(seen.logo?.startsWith("data:image/svg+xml"), "the logo is in the sidebar header");
    assert.notEqual(seen.accent, scheme === "light" ? "#e9eef5" : "#1f2a4d", `the accent colour changes the accent token (${scheme})`);
    const lum = (hex) => [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255).map((v) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4)).reduce((a, v, i) => a + v * [0.2126, 0.7152, 0.0722][i], 0);
    const [x, y] = [lum(seen.accent), lum(seen.foreground)];
    assert.ok((Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05) >= 4.5, `text on the accent surface reads (${scheme}): ${seen.accent} on ${seen.foreground}`);
  }
  await context.close();
});

test("the shell: service switcher, colour ramp, favicon, environment chip, command search and user footer", async () => {
  const { context, page } = await signedIn();
  // The metadata route is stubbed with a switcher; the Rust side has its own tests.
  await page.route(/\/api\/site$/, async (route) => {
    const response = await route.fetch();
    const body = await response.json();
    body.brand = {
      ...body.brand, name: "Acme", environment: "Production", environment_level: null, colors: { primary: "#7c3aed" },
      switcher: { current: "app", subheader: "v1.2 · main", rows: ["build 42"], build: "42", services: [
        { key: "app", title: "Application", url: null, icon: "layout-dashboard", color: "#7c3aed" },
        { key: "billing", title: "Billing", url: "/billing/admin/", icon: "credit-card", color: "#0b7a5b" },
      ] },
    };
    await route.fulfill({ response, json: body });
  });
  await page.goto(`${base}/admin/`);
  const aside = page.locator("#sidebar");
  await aside.getByRole("button", { name: /switch service/ }).click();
  const menu = page.getByRole("menu");
  assert.equal(await menu.getByRole("menuitem").count(), 2, "one row per service");
  assert.equal(await menu.getByRole("menuitem", { name: /Application/ }).getAttribute("aria-disabled"), "true", "the current row is inert");
  assert.equal(await menu.getByRole("menuitem", { name: /Billing/ }).getAttribute("href"), "/billing/admin/");
  assert.ok(await menu.getByText("build 42").isVisible(), "the build rows show");
  await page.keyboard.press("Escape");

  const seen = await page.evaluate(() => {
    const css = getComputedStyle(document.documentElement);
    return { p600: css.getPropertyValue("--primary-600").trim(), p500: css.getPropertyValue("--primary-500").trim(), icon: document.querySelector('link[rel="icon"]')?.getAttribute("href") ?? "" };
  });
  assert.match(seen.p600, /^oklch\(/, "the ramp is set from the service colour");
  assert.notEqual(seen.p600, seen.p500);
  assert.ok(seen.icon.startsWith("data:image/svg+xml"), "the favicon is a data URL");
  assert.equal(await page.getByTestId("page-title").innerText(), "Home");
  assert.equal(await page.locator("[data-level]").getAttribute("data-level"), "danger", "Production defaults to the danger level");

  // Ctrl+K opens the search; a typed name and Enter open the list of the resource.
  await page.keyboard.press("Control+k");
  const box = page.getByRole("combobox", { name: "Search the resources" });
  await box.fill("reviewer");
  await box.press("Enter");
  await page.waitForURL(`${base}/admin/Reviewer`);
  assert.equal(await page.getByTestId("page-title").innerText(), "Reviewer");

  // The footer shows the user and opens the account menu.
  const footer = aside.getByRole("button", { name: "User menu" });
  assert.ok((await footer.innerText()).includes("ann"));
  await footer.click();
  await page.getByRole("menuitem", { name: "Sign out" }).waitFor();
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
  // The panel takes the focus on open, and the trigger reports the state.
  const panel = page.getByRole("group", { name: "Filters" });
  await panel.waitFor();
  assert.equal(await filtersButton.getAttribute("aria-expanded"), "true");
  assert.ok(await panel.evaluate((e) => e.contains(document.activeElement)), "the focus is in the panel");
  // The keyboard reaches the collapsible picker.
  const choose = page.getByRole("button", { name: "Choose Author" });
  assert.equal(await choose.getAttribute("aria-expanded"), "false");
  await choose.focus();
  await page.keyboard.press("Enter");
  await page.getByRole("button", { name: "Ada", exact: true }).click();
  await page.waitForURL(`${base}/admin/Book?f.author=1`);
  await rowsAre(["First"]);
  await page.keyboard.press("Escape");
  await panel.waitFor({ state: "detached" });
  await filtersButton.evaluate((e) => new Promise((ok, no) => { const t = Date.now(); const tick = () => (e === document.activeElement ? ok() : Date.now() - t > 2000 ? no(new Error("Escape does not return the focus to the trigger")) : setTimeout(tick, 20)); tick(); }));
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
  await page.getByRole("navigation", { name: "Breadcrumb" }).getByRole("link", { name: "Book", exact: true }).click();
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

test("an error summary links to the field, a sticky bar stays in view, and unsaved changes ask before the page changes", async () => {
  const { context, page, problems, idle } = await signedIn();
  const asked = [];
  let accept = false;
  page.on("dialog", (d) => {
    asked.push(d.message());
    return accept ? d.accept() : d.dismiss();
  });
  await page.goto(`${base}/admin/Event/1/edit`);
  await page.getByRole("heading", { name: "Change Event" }).waitFor();
  // A form nobody changed lets the user leave at once.
  await page.getByRole("link", { name: "Cancel" }).click();
  await page.waitForURL(`${base}/admin/Event/1`);
  assert.deepEqual(asked, []);
  await idle();

  await page.goto(`${base}/admin/Event/1/edit`);
  const bar = page.locator(".savebar");
  assert.equal(await bar.evaluate((e) => getComputedStyle(e).position), "sticky");
  const box = await bar.boundingBox();
  assert.ok(box && box.y + box.height <= page.viewportSize().height + 1, "the save bar is in the viewport");

  // A bad JSON is a field error: the summary names the field and its link moves the focus there.
  const extra = page.getByLabel("Extra");
  await extra.fill('{"a":');
  await page.getByRole("button", { name: "Save", exact: true }).click();
  const summary = page.getByRole("group", { name: "1 field needs attention" });
  await summary.waitFor();
  assert.ok(await summary.evaluate((e) => e === document.activeElement), "the summary takes the focus");
  await page.getByRole("link", { name: "Extra" }).click();
  assert.ok(await extra.evaluate((e) => e === document.activeElement), "the link focuses the field");

  // The page asks before it goes away. Cancel keeps the page.
  await page.getByRole("link", { name: "Cancel" }).click();
  await page.waitForTimeout(200);
  assert.equal(page.url(), `${base}/admin/Event/1/edit`);
  assert.equal(asked.length, 1);
  assert.match(asked[0], /unsaved changes/);
  accept = true;
  await page.getByRole("link", { name: "Cancel" }).click();
  await page.waitForURL(`${base}/admin/Event/1`);
  assert.equal(asked.length, 2);
  await idle();

  // After a save the page goes away with no question.
  await page.goto(`${base}/admin/Event/1/edit`);
  await page.getByLabel("Title").fill("Launch again");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.waitForURL(`${base}/admin/Event/1`);
  assert.equal(asked.length, 2);
  await idle();

  // A save that keeps the form open makes the saved values the new start: Cancel asks nothing.
  await page.goto(`${base}/admin/Event/1/edit`);
  await page.getByLabel("Title").fill("Launch once more");
  const saved = page.waitForResponse((r) => r.request().method() !== "GET" && r.url().includes("/Event/1"));
  await page.getByRole("button", { name: "Save and continue" }).click();
  await saved;
  await idle();
  await page.getByRole("link", { name: "Cancel" }).click();
  await page.waitForURL(`${base}/admin/Event/1`);
  assert.equal(asked.length, 2, "no question after Save and continue");
  await idle();

  // The same for an empty form that a save clears for the next record.
  await page.goto(`${base}/admin/Author/-/new`);
  await page.getByLabel("Name").fill("Next Author");
  const added = page.waitForResponse((r) => r.request().method() === "POST" && r.url().includes("/Author"));
  await page.getByRole("button", { name: "Save and add another" }).click();
  await added;
  await idle();
  await page.getByRole("link", { name: "Cancel" }).click();
  await page.waitForURL(`${base}/admin/Author`);
  assert.equal(asked.length, 2, "no question after Save and add another");
  await idle();

  // Browser Back is a page change inside the app: it asks as well, and a refusal keeps the page.
  accept = false;
  await page.goto(`${base}/admin/Event/1`);
  await page.getByRole("link", { name: "Edit" }).click();
  await page.waitForURL(`${base}/admin/Event/1/edit`);
  await page.getByLabel("Title").fill("Back test");
  await page.evaluate(() => history.back());
  await page.waitForTimeout(300);
  assert.equal(page.url(), `${base}/admin/Event/1/edit`);
  assert.equal(asked.length, 3);
  assert.match(asked[2], /unsaved changes/);
  assert.equal(await page.getByLabel("Title").inputValue(), "Back test", "the form keeps its edit");
  accept = true;
  await page.evaluate(() => history.back());
  // The browser changes the URL before popstate runs, so wait for the question itself.
  await page.waitForFunction(() => document.querySelector("h1")?.textContent?.includes("Event") && location.pathname.endsWith("/Event/1"));
  for (let i = 0; i < 50 && asked.length < 4; i++) await page.waitForTimeout(50);
  assert.equal(asked.length, 4);
  assert.equal(new URL(page.url()).pathname, "/admin/Event/1");
  await idle();
  assert.deepEqual(problems, []);
  await context.close();
});

/** Wait until the page has asked `n` questions: a browser Back changes the URL before `popstate` runs. */
async function asks(asked, n, page) {
  for (let i = 0; i < 60 && asked.length < n; i++) await page.waitForTimeout(50);
  assert.equal(asked.length, n);
}

/** A stack of four entries on Event 1 — list, detail, edit, detail — and a dirty edit form one step back from the end. */
async function dirtyEditBeforeForward(page, idle) {
  await page.goto(`${base}/admin/Event`);
  await page.getByRole("link", { name: "1", exact: true }).click();
  await page.waitForURL(`${base}/admin/Event/1`);
  await page.getByRole("link", { name: "Edit" }).click();
  await page.waitForURL(`${base}/admin/Event/1/edit`);
  await page.getByRole("link", { name: "Cancel" }).click();
  await page.waitForURL(`${base}/admin/Event/1`);
  await idle();
  await page.evaluate(() => history.back());
  await page.waitForURL(`${base}/admin/Event/1/edit`);
  await page.getByLabel("Title").fill("Forward test");
}

test("a refused Forward keeps the stack: the page stays, Forward still works, and Back lands one entry back", async () => {
  const { context, page, problems, idle } = await signedIn();
  const asked = [];
  let accept = false;
  page.on("dialog", (d) => (asked.push(d.message()), accept ? d.accept() : d.dismiss()));
  await dirtyEditBeforeForward(page, idle);
  await page.evaluate(() => history.forward());
  await asks(asked, 1, page);
  await page.waitForTimeout(200);
  assert.equal(new URL(page.url()).pathname, "/admin/Event/1/edit", "the refused Forward leaves the page");
  assert.equal(await page.getByLabel("Title").inputValue(), "Forward test", "the form keeps its edit");
  // The entry after the page is still there: an accepted Forward reaches it.
  accept = true;
  await page.evaluate(() => history.forward());
  await asks(asked, 2, page);
  await page.waitForURL(`${base}/admin/Event/1`);
  await idle();
  assert.deepEqual(problems, []);
  await context.close();
});

test("Back after a refused Forward goes to the entry before the page, not to the page again", async () => {
  const { context, page, problems, idle } = await signedIn();
  const asked = [];
  let accept = false;
  page.on("dialog", (d) => (asked.push(d.message()), accept ? d.accept() : d.dismiss()));
  await dirtyEditBeforeForward(page, idle);
  await page.evaluate(() => history.forward());
  await asks(asked, 1, page);
  await page.waitForTimeout(200);
  accept = true;
  await page.evaluate(() => history.back());
  await asks(asked, 2, page);
  await page.waitForURL(`${base}/admin/Event/1`);
  await idle();
  // One more Back reaches the list: the stack holds no copy of the edit page.
  await page.evaluate(() => history.back());
  await page.waitForURL(`${base}/admin/Event`);
  assert.equal(asked.length, 2, "a clean page asks nothing");
  await idle();
  assert.deepEqual(problems, []);
  await context.close();
});

test("a radio group takes the focus from the error summary, and a datetime that is edited and set back asks nothing", async () => {
  const { context, page, problems, idle } = await signedIn();
  const asked = [];
  page.on("dialog", (d) => (asked.push(d.message()), d.dismiss()));
  // A radio group with no value: the server refuses the save and the summary links to the group.
  await page.goto(`${base}/admin/Gadget/-/new`);
  await page.getByLabel("Name").fill("Gizmo");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.getByRole("group", { name: "1 field needs attention" }).waitFor();
  assert.equal(await page.locator("form [role=alert]").count(), 0, "the failure shows under the summary only, not again as a message");
  const group = page.getByRole("radiogroup", { name: "Tier" });
  await page.getByRole("link", { name: "Tier" }).click();
  assert.ok(await group.evaluate((e) => e === document.activeElement), "the link focuses the radio group");
  await group.getByLabel("pro").check();
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.waitForURL(/\/admin\/Gadget\/\d+$/);
  assert.equal(asked.length, 0);
  await idle();

  // Event 2 stores whole minutes: the browser drops the zero seconds, so a value that goes out and back must still equal the start.
  await page.goto(`${base}/admin/Event/2/edit`);
  const starts = page.getByLabel("Starts");
  const at = page.getByLabel("At", { exact: true });
  assert.equal(await starts.inputValue(), "2026-01-02T07:30");
  assert.equal(await at.inputValue(), "10:00");
  await starts.fill("2026-01-02T07:45");
  await at.fill("10:15");
  await starts.fill("2026-01-02T07:30");
  await at.fill("10:00");
  await page.getByRole("link", { name: "Cancel" }).click();
  await page.waitForURL(`${base}/admin/Event/2`);
  assert.equal(asked.length, 0, "no question for a value that is back at the start");
  await idle();
  assert.deepEqual(problems, [`HTTP 422: ${base}/admin/api/resources/Gadget`, "console: Failed to load resource: the server responded with a status of 422 (Unprocessable Entity)"]);
  await context.close();
});

test("a new record saved with Save and continue opens its edit page, and the form there asks about its own changes", async () => {
  const { context, page, problems, idle } = await signedIn();
  const asked = [];
  page.on("dialog", (d) => (asked.push(d.message()), d.dismiss()));
  await page.goto(`${base}/admin/Gadget/-/new`);
  await page.getByLabel("Name").fill("Continued");
  await page.getByRole("radiogroup", { name: "Tier" }).getByLabel("pro").check();
  await page.getByRole("button", { name: "Save and continue" }).click();
  await page.waitForURL(/\/admin\/Gadget\/\d+\/edit$/);
  await page.getByRole("heading", { name: "Change Gadget" }).waitFor();
  assert.equal(await page.getByLabel("Name").inputValue(), "Continued");
  assert.equal(asked.length, 0, "the save leaves with no question");
  await idle();
  // The edit page starts clean: Cancel asks nothing. A change makes it ask.
  await page.getByRole("link", { name: "Cancel" }).click();
  await page.waitForURL(/\/admin\/Gadget\/\d+$/);
  assert.equal(asked.length, 0, "a clean edit page asks nothing");
  await page.getByRole("link", { name: "Edit" }).click();
  await page.waitForURL(/\/admin\/Gadget\/\d+\/edit$/);
  await page.getByLabel("Name").fill("Changed");
  await page.getByRole("link", { name: "Cancel" }).click();
  await page.waitForTimeout(200);
  assert.equal(asked.length, 1);
  assert.match(page.url(), /\/edit$/);
  await idle();
  assert.deepEqual(problems, []);
  await context.close();
});

test("closing the tab asks while the form has unsaved changes, and not after a save", async () => {
  const { context, page, idle } = await signedIn();
  const kinds = [];
  page.on("dialog", (d) => (kinds.push(d.type()), d.dismiss()));
  await page.goto(`${base}/admin/Event/1/edit`);
  await page.getByRole("heading", { name: "Change Event" }).waitFor();
  // A clean form closes with no question.
  const other = await context.newPage();
  await other.goto(`${base}/admin/Event/1/edit`);
  await other.getByRole("heading", { name: "Change Event" }).waitFor();
  await other.close({ runBeforeUnload: true });
  assert.deepEqual(kinds, []);
  // A user gesture is needed before the browser shows the question.
  await page.getByLabel("Title").click();
  await page.keyboard.type("x");
  await page.close({ runBeforeUnload: true });
  for (let i = 0; i < 40 && kinds.length < 1; i++) await new Promise((r) => setTimeout(r, 50));
  assert.deepEqual(kinds, ["beforeunload"]);
  assert.equal(page.isClosed(), false, "the user stayed");
  await page.close();
  await context.close();
});

test("a section layout with a collapsed section, and a custom form", async () => {
  const { context, page, problems, idle } = await signedIn();
  await page.goto(`${base}/admin/Book/-/new`);
  await page.getByRole("heading", { name: "Add Book" }).waitFor();
  await page.getByText("Basics", { exact: true }).waitFor();
  const more = page.getByRole("button", { name: "More", exact: true });
  assert.equal(await more.getAttribute("aria-expanded"), "false", "the collapsed section starts closed");
  assert.equal(await page.getByLabel("Author id").isVisible(), false);
  // The keyboard opens it and closes it again, and the focus stays on the button.
  await more.focus();
  await page.keyboard.press("Enter");
  await page.getByLabel("Author id").waitFor();
  assert.equal(await more.getAttribute("aria-expanded"), "true");
  await page.keyboard.press("Space");
  await page.getByLabel("Author id").waitFor({ state: "hidden" });
  assert.ok(await more.evaluate((e) => e === document.activeElement));
  await more.click();
  await page.getByLabel("Author id").waitFor();
  // The fields that no section names get a title beside the titled cards.
  assert.equal(await page.locator("form fieldset").count(), 2);
  assert.equal(await page.locator("form fieldset legend").last().innerText(), "Other");

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

  // Escape and Cancel ask before they discard typed values; an untouched dialog closes at once.
  await idle();
  await page.goto(`${base}/admin/Paper/-/new`);
  const asked = [];
  let accept = false;
  page.on("dialog", (d) => {
    asked.push(d.message());
    return accept ? d.accept() : d.dismiss();
  });
  const open = async () => {
    await page.getByRole("combobox", { name: "Reviewer id" }).click();
    await page.getByRole("button", { name: /^New Reviewer/ }).click();
    const d = page.getByRole("dialog", { name: "New Reviewer" });
    await d.waitFor();
    return d;
  };
  let guarded = await open();
  await page.keyboard.press("Escape");
  await guarded.waitFor({ state: "detached" });
  assert.deepEqual(asked, [], "an untouched dialog closes without a question");
  guarded = await open();
  await guarded.getByLabel("Name").fill("Draft");
  await page.keyboard.press("Escape");
  await page.waitForTimeout(200);
  assert.equal(asked.length, 1);
  assert.match(asked[0], /unsaved changes/);
  assert.equal(await guarded.getByLabel("Name").inputValue(), "Draft", "a refused Escape keeps the typed value");
  await guarded.getByRole("button", { name: "Cancel" }).click();
  await page.waitForTimeout(200);
  assert.equal(asked.length, 2, "Cancel asks too");
  assert.equal(await guarded.isVisible(), true);
  accept = true;
  await guarded.getByRole("button", { name: "Cancel" }).click();
  await guarded.waitFor({ state: "detached" });
  assert.equal(asked.length, 3);
  page.removeAllListeners("dialog");
  await idle();

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
  const tracks = page.locator(".inline");
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

test("a picker inside an inline table opens its whole list, not a clipped one", async () => {
  const { context, page, problems, idle } = await signedIn();
  await page.goto(`${base}/admin/Album/1/edit`);
  await page.getByRole("heading", { name: "Change Album" }).waitFor();
  const tracks = page.locator(".inline");
  await tracks.getByText("Tracks", { exact: true }).waitFor();
  await tracks.getByRole("combobox").first().click();
  const list = page.getByRole("listbox");
  await list.getByRole("option").nth(5).waitFor();
  // The point at the bottom edge of the list must hit the list: a table wrapper with `overflow-x: auto` would cut it off.
  const hit = await list.evaluate((e) => {
    const r = e.parentElement.getBoundingClientRect();
    const el = document.elementFromPoint(r.left + r.width / 2, r.bottom - 3);
    return el !== null && e.parentElement.contains(el);
  });
  assert.ok(hit, "the bottom of the list is on top of the page");
  await idle();
  assert.deepEqual(problems, []);
  await context.close();
});

test("a detail page shows a card for each section of the resource, and one Details card when it has none", async () => {
  const { context, page, problems, idle } = await signedIn();
  await page.goto(`${base}/admin/Book/1`);
  await page.getByRole("region", { name: "Basics" }).waitFor();
  await page.getByRole("region", { name: "More" }).waitFor();
  await page.getByRole("region", { name: "Other" }).waitFor();
  assert.equal(await page.getByRole("region", { name: "Details" }).count(), 0, "a resource with sections has no Details card");
  await page.getByRole("region", { name: "Basics" }).getByText("Title", { exact: true }).waitFor();
  await page.goto(`${base}/admin/Event/1`);
  await page.getByRole("region", { name: "Details" }).waitFor();
  assert.equal(await page.getByRole("region").count(), 1, "one card holds every field");
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

  await page.goto(`${base}/admin/Ticket`);
  await page.getByLabel("Select row 1").check();
  await page.getByRole("button", { name: "Close", exact: true }).click();
  await page.getByRole("alertdialog", { name: "Confirm Close" }).getByRole("button", { name: "Close", exact: true }).click();
  await page.getByRole("status").filter({ hasText: "Closed 1 tickets" }).waitFor();

  // The closed ticket refuses edits and review, and its reopen action can still run.
  await page.goto(`${base}/admin/Ticket/1`);
  const review = page.getByRole("button", { name: "Review", exact: true });
  await review.waitFor();
  assert.equal(await review.isDisabled(), true);
  assert.equal(await review.getAttribute("title"), "You may not run this action on this record");
  assert.equal(await page.getByRole("button", { name: "Edit", exact: true }).isDisabled(), true);
  await page.goto(`${base}/admin/Ticket`);
  await page.getByRole("cell", { name: "Ticket 1" }).waitFor();
  await page.getByRole("button", { name: "Actions for row 1" }).click();
  await page.getByRole("menuitem", { name: "Reopen" }).click();
  await page.getByRole("status").filter({ hasText: "Reopened" }).waitFor();
  await page.getByRole("button", { name: "Actions for row 1" }).click();
  const reopen = page.getByRole("menuitem", { name: "Reopen" });
  assert.equal(await reopen.getAttribute("aria-disabled"), "true");
  assert.equal(await reopen.getAttribute("title"), "You may not run this action on this record");
  await page.keyboard.press("Escape");
  await page.goto(`${base}/admin/Ticket/1`);
  await review.waitFor();
  assert.equal(await review.isEnabled(), true);
  await page.getByRole("link", { name: "Edit", exact: true }).click();
  const submit = page.getByRole("button", { name: "Save and mark closed" });
  await submit.waitFor();
  assert.equal(await submit.isDisabled(), true);
  assert.equal(await submit.getAttribute("title"), "You may not run this action on this record");
  await idle();
  assert.deepEqual(problems, []);
  await context.close();
});

test("record banners show under the tabs and above the sections, and a list read has none", async () => {
  const { page, idle } = await signedIn();
  await page.goto(`${base}/admin/Book/1`);
  const banners = page.getByTestId("banners");
  await banners.waitFor();
  assert.equal(await banners.getByRole("status").count(), 2, "an info and a warning banner");
  await banners.getByText("Book 1 is in the seed").waitFor();
  await banners.getByText("Check the author").waitFor();
  const above = await banners.evaluate((el) => {
    const card = document.querySelector("section [role=region]");
    return !card || !!(el.compareDocumentPosition(card) & Node.DOCUMENT_POSITION_FOLLOWING);
  });
  assert.ok(above, "the banners come before the first section");
  await idle();
  await page.goto(`${base}/admin/Book`);
  await page.getByRole("heading", { name: /Book/ }).waitFor();
  assert.equal(await page.getByTestId("banners").count(), 0);
});

test("detail v2: breadcrumbs, the header bar, a folding section and the sticky footer", async () => {
  const { context, page, problems, idle } = await signedIn();
  await page.evaluate(() => localStorage.clear());
  await page.goto(`${base}/admin/Book/1`);
  await page.getByRole("heading", { name: /Book/ }).waitFor();
  const crumbs = page.getByRole("navigation", { name: "Breadcrumb" });
  await crumbs.getByRole("link", { name: "Home" }).waitFor();
  assert.equal(await crumbs.getByRole("link", { name: "Book", exact: true }).count(), 1);
  assert.equal(await crumbs.locator("[aria-current=page]").count(), 1);

  // The header holds the primary plus and a More menu with the built-in History item.
  await page.getByRole("link", { name: "Add", exact: true }).waitFor();
  await page.getByRole("group", { name: "Actions", exact: true }).getByRole("button", { name: "More", exact: true }).click();
  const audit = page.waitForResponse((r) => r.request().method() === "GET" && /\/resources\/AdminLogEntry\?/.test(r.url()));
  await page.getByRole("menuitem", { name: "History" }).click();
  await page.waitForURL(/\/admin\/AdminLogEntry\?.*f\.resource=Book.*f\.record_key=1|\/admin\/AdminLogEntry\?.*f\.record_key=1.*f\.resource=Book/);
  const logged = await audit;
  assert.equal(logged.status(), 200, "the audit list accepts the History filters");
  const entries = (await logged.json()).rows;
  assert.ok(entries.every((row) => row.includes("Book") && row.includes("1")), "every row shown is a row of Book 1 (the list may hold none when this case runs alone)");
  // The group of the resource is the first crumb after Home, as text.
  const trail = page.getByRole("navigation", { name: "Breadcrumb" });
  await trail.getByText("Users and access", { exact: true }).waitFor();
  assert.equal(await trail.getByRole("link", { name: "Users and access" }).count(), 0);
  await idle();

  // A section with `collapsed` starts folded, opens on a click and keeps its state after a reload.
  await page.goto(`${base}/admin/Book/1`);
  const fold = page.getByRole("region", { name: "More" }).getByRole("button", { name: "More", exact: true });
  await fold.waitFor();
  assert.equal(await fold.getAttribute("aria-expanded"), "false");
  await fold.click();
  assert.equal(await fold.getAttribute("aria-expanded"), "true");
  await page.reload();
  const again = page.getByRole("region", { name: "More" }).getByRole("button", { name: "More", exact: true });
  await again.waitFor();
  assert.equal(await again.getAttribute("aria-expanded"), "true", "the fold state survives a reload");

  // The detail footer: Delete on the left, Edit on the right, and the header has no Delete.
  const footer = page.getByRole("group", { name: "Page actions" });
  const del = footer.getByRole("button", { name: "Delete", exact: true });
  const edit = footer.getByRole("link", { name: "Edit", exact: true });
  await del.waitFor();
  const [d, e] = [await del.boundingBox(), await edit.boundingBox()];
  assert.ok(d && e && d.x < e.x, "Delete is left of Edit");
  assert.equal(await page.getByRole("button", { name: "Delete", exact: true }).count(), 1);

  // The form footer: a variant action on the right of Delete, and breadcrumbs.
  await page.goto(`${base}/admin/Ticket/1/edit`);
  const bar = page.getByRole("group", { name: "Page actions" });
  const mark = bar.getByRole("button", { name: "Save and mark closed" });
  await mark.waitFor();
  assert.equal(await bar.evaluate((el) => getComputedStyle(el).position), "sticky");
  const left = await bar.getByRole("button", { name: "Delete", exact: true }).boundingBox();
  const right = await mark.boundingBox();
  assert.ok(left && right && left.x < right.x, "Delete is left of the variant action");
  await page.getByRole("navigation", { name: "Breadcrumb" }).getByRole("link", { name: "Ticket", exact: true }).waitFor();
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
  const module = readFileSync(join(copy, "admin.py"), "utf8");
  assert.ok(module.includes("admin.register(User)"));
  writeFileSync(join(copy, "admin.py"), module.replace("admin.register(User)", 'admin.register(User, {"set_password": True, "list": {"columns": [User.username]}})'));
  const toml = readFileSync(join(copy, "monorm.toml"), "utf8");
  writeFileSync(join(copy, "monorm.toml"), `${toml.trimEnd()}\n\n[admin.oidc]\nissuer = "${idp.issuer}"\nclient_id = "${CLIENT_ID}"\nhd = "example.com"\n`);
  const env = { ...process.env, PYTHONPATH: ROOT, MONORM_DATABASE_URL: `sqlite://${copy}/app.db`, MONORM_ADMIN_SECRET: SECRET, MONORM_ADMIN_PORT: "0", MONORM_ADMIN_OIDC_CLIENT_SECRET: CLIENT_SECRET };
  const other = spawn(PYTHON, ["-m", "monorm", "admin"], { cwd: copy, env, stdio: ["ignore", "ignore", "pipe"] });
  let text = "";
  try {
    const url = await new Promise((resolveUrl, reject) => {
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

    const adminContext = await browser.newContext();
    try {
      const adminPage = await adminContext.newPage();
      await adminPage.goto(`${url}/admin/-/login`);
      await adminPage.getByLabel("Username").fill("ann");
      await adminPage.getByLabel("Password", { exact: true }).fill("pw-of-ann");
      await adminPage.getByRole("button", { name: "Sign in", exact: true }).click();
      await adminPage.getByRole("navigation", { name: "Resources" }).waitFor();
      const userKey = (await me.json()).user.key;
      await adminPage.goto(`${url}/admin/User/${userKey}`);
      await adminPage.getByRole("button", { name: "Set password", exact: true }).click();
      const dialog = adminPage.getByRole("alertdialog");
      await dialog.getByText("Set a new password for sso@example.com? All sessions of this user end.").waitFor();
      await dialog.getByText("This user signs in with single sign-on. A password gives a second way in.").waitFor();
      const passwordField = dialog.locator('input[name="new_password"]');
      const repeatedField = dialog.getByLabel(/^New password again/);
      for (const field of [passwordField, repeatedField]) {
        assert.equal(await field.getAttribute("type"), "password");
        assert.equal(await field.getAttribute("autocomplete"), "new-password");
        await field.fill("1234");
      }
      await dialog.getByRole("button", { name: "Set password", exact: true }).click();
      await dialog.getByText("This password is too short.", { exact: false }).waitFor();
      assert.equal(await passwordField.getAttribute("aria-invalid"), "true");
      const password = "private-sso-reset-password";
      await passwordField.fill(password);
      await repeatedField.fill(password);
      await dialog.getByRole("button", { name: "Set password", exact: true }).click();
      await dialog.waitFor({ state: "hidden" });
      assert.equal((await context.request.get(`${url}/admin/api/me`)).status(), 401);
      await page.goto(`${url}/admin/-/login`);
      await page.getByLabel("Username").fill("sso@example.com");
      await page.getByLabel("Password", { exact: true }).fill(password);
      await page.getByRole("button", { name: "Sign in", exact: true }).click();
      await page.waitForURL((next) => next.origin === url && next.pathname.replace(/\/$/, "") === "/admin");
      const passwordSession = await context.request.get(`${url}/admin/api/me`);
      assert.equal(passwordSession.status(), 200);
      assert.equal((await passwordSession.json()).user.username, "sso@example.com");
      assert.ok(text.includes("/admin/api/actions/User/set_password"));
      assert.ok(!text.includes(password), "the password stays out of the server's request logs");
    } finally {
      await adminContext.close();
      await context.close();
    }
  } finally {
    other.removeAllListeners("exit");
    other.kill("SIGTERM");
    await idp.close();
    rmSync(copy, { recursive: true, force: true });
  }
});

test("single sign-on with auto_start: no form, the requested page after the provider, the form at the break-glass URLs and no loop", async () => {
  const idp = await startIdp({ claims: { email: "auto@example.com", roles: ["support"], hd: "example.com" } });
  const copy = `${dir}-sso-auto`;
  cpSync(dir, copy, { recursive: true });
  const toml = readFileSync(join(copy, "monorm.toml"), "utf8");
  writeFileSync(join(copy, "monorm.toml"), `${toml.trimEnd()}\n\n[admin.oidc]\nissuer = "${idp.issuer}"\nclient_id = "${CLIENT_ID}"\nhd = "example.com"\nauto_start = true\n`);
  const env = { ...process.env, PYTHONPATH: ROOT, MONORM_DATABASE_URL: `sqlite://${copy}/app.db`, MONORM_ADMIN_SECRET: SECRET, MONORM_ADMIN_PORT: "0", MONORM_ADMIN_OIDC_CLIENT_SECRET: CLIENT_SECRET };
  const other = spawn(PYTHON, ["-m", "monorm", "admin"], { cwd: copy, env, stdio: ["ignore", "ignore", "pipe"] });
  let text = "";
  try {
    const url = await new Promise((resolveUrl, reject) => {
      other.stderr.on("data", (chunk) => {
        text += chunk;
        const found = /"url":"([^"]+)"/.exec(text);
        if (found && text.includes('"listening"')) resolveUrl(found[1]);
      });
      other.on("exit", (code) => reject(new Error(`the admin ended with ${code}\n${text}`)));
    });
    const context = await browser.newContext({ timezoneId: ZONE, locale: "en-US" });
    const page = await context.newPage();
    const starts = [];
    page.on("request", (r) => r.url().includes("/admin/auth/start") && starts.push(r.url()));
    // No session: the browser goes to the provider and returns to the page that it asked for.
    await page.goto(`${url}/admin/Author`);
    await page.getByRole("button", { name: "User menu" }).waitFor();
    assert.equal(page.url(), `${url}/admin/Author`);
    assert.equal((await context.request.get(`${url}/admin/api/me`)).status(), 200, "signed in with no form");
    assert.equal(starts.length, 1);
    assert.ok(decodeURIComponent(starts[0]).endsWith("next=/admin/Author"), starts[0]);

    // A session that ends while the page is open (a 401) starts the flow again with the current page as `next`.
    await context.clearCookies();
    await page.evaluate(() => window.sessionStorage.clear());
    // A page that mounts again reads its data again: the 401 comes from that query.
    await page.getByRole("link", { name: "Admin", exact: true }).click();
    await page.waitForURL(`${url}/admin`);
    const restarted = page.waitForRequest((r) => r.url().includes("/admin/auth/start"));
    await page.goBack();
    await restarted;
    await page.getByRole("button", { name: "User menu" }).waitFor();
    assert.equal(page.url(), `${url}/admin/Author`);
    assert.equal(starts.length, 2, "the 401 starts the flow once");
    assert.ok(decodeURIComponent(starts[1]).endsWith("next=/admin/Author"), starts[1]);

    // After the sign-out the form shows, and it does not start the flow again.
    await page.getByRole("button", { name: "User menu" }).click();
    await page.getByRole("menuitem", { name: "Sign out" }).click();
    await page.waitForURL(`${url}/admin/-/login`);
    await page.getByLabel("Username").waitFor();
    assert.equal(starts.length, 2, "the page after sign-out starts no flow");

    // The break-glass URL and a failed callback show the form with no redirect.
    const fresh = await browser.newContext({ timezoneId: ZONE, locale: "en-US" });
    try {
      const guest = await fresh.newPage();
      const guestStarts = [];
      guest.on("request", (r) => r.url().includes("/admin/auth/start") && guestStarts.push(r.url()));
      await guest.goto(`${url}/admin/?password=1`);
      await guest.getByLabel("Username").waitFor();
      await guest.goto(`${url}/admin/?sso_error=domain`);
      await guest.getByText("This account is not in the allowed domain.").waitFor();
      await guest.getByLabel("Username").waitFor();
      assert.equal(guestStarts.length, 0, "neither page starts the flow");
    } finally {
      await fresh.close();
    }

    // A `next` outside the base path is dropped: the user lands on the home page.
    const outside = await browser.newContext({ timezoneId: ZONE, locale: "en-US" });
    try {
      const visitor = await outside.newPage();
      await visitor.goto(`${url}/admin/auth/start?next=${encodeURIComponent("https://evil.example/")}`);
      await visitor.waitForURL((next) => next.origin === url && next.pathname.replace(/\/$/, "") === "/admin");
    } finally {
      await outside.close();
    }
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

async function signIn(page) {
  await page.goto(`${base}/admin/-/login`);
  await page.getByLabel("Username").fill("ann");
  await page.getByLabel("Password").fill("pw-of-ann");
  await page.getByRole("button", { name: "Sign in" }).click();
  await page.getByRole("navigation", { name: "Resources" }).waitFor();
}

// Badges and the bool icon: text contrast of at least 4.5:1 in both themes, and no inline style (the page policy forbids it).
for (const scheme of ["light", "dark"]) {
  test(`a list shows a bool as an icon and a column as badges, with readable text (${scheme})`, async () => {
    const context = await browser.newContext({ timezoneId: ZONE, locale: "en-US", colorScheme: scheme });
    const page = await context.newPage();
    const problems = [];
    page.on("console", (m) => m.type() === "error" && problems.push(`console: ${m.text()}`));
    page.on("pageerror", (e) => problems.push(`page error: ${e.message}`));
    page.on("response", (r) => r.status() >= 400 && r.status() !== 401 && problems.push(`HTTP ${r.status()}: ${r.url()}`));
    await signIn(page);
    await page.goto(`${base}/admin/Parcel`);
    await page.getByRole("cell", { name: "P-2024-03" }).waitFor();

    assert.equal(await page.getByRole("img", { name: "Yes" }).count(), 3, "three paid parcels");
    assert.equal(await page.getByRole("img", { name: "No" }).count(), 2, "two unpaid parcels");
    const badges = await page.locator("tbody span[class*='badge-']").evaluateAll((els) =>
      els.map((el) => {
        const own = getComputedStyle(el);
        return { text: el.textContent.trim(), tone: [...el.classList].find((c) => c.startsWith("badge-")), fg: own.color, bg: own.backgroundColor, inline: el.hasAttribute("style") };
      }));
    assert.deepEqual(badges.map((b) => [b.text, b.tone]), [["shipped", "badge-green"], ["new", "badge-blue"], ["lost", "badge-red"], ["returned", "badge-grey"], ["shipped", "badge-green"]], "a value outside the map is grey");
    assert.equal(badges.some((b) => b.inline), false, "no inline style");
    const lum = (css) => {
      const [r, g, b] = css.match(/[\d.]+/g).slice(0, 3).map((v) => Number(v) / 255).map((v) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4));
      return 0.2126 * r + 0.7152 * g + 0.0722 * b;
    };
    for (const b of badges) {
      const [x, y] = [lum(b.fg), lum(b.bg)];
      const ratio = (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
      assert.ok(ratio >= 4.5, `badge ${b.tone} reads (${scheme}): ${b.fg} on ${b.bg} is ${ratio.toFixed(2)}`);
    }

    // The detail page shows the same badge.
    await page.getByRole("link", { name: "P-2024-03" }).click();
    await page.getByText("shipped").first().waitFor();
    assert.equal(await page.locator("span.badge-green").filter({ hasText: "shipped" }).count(), 1);
    assert.deepEqual(problems, []);
    await context.close();
  });
}

test("a date drill-down: a click on a year and then on a month narrows the rows and the chip", async () => {
  const context = await browser.newContext({ timezoneId: ZONE, locale: "en-US" });
  const page = await context.newPage();
  const problems = [];
  page.on("console", (m) => m.type() === "error" && problems.push(`console: ${m.text()}`));
  page.on("pageerror", (e) => problems.push(`page error: ${e.message}`));
  page.on("response", (r) => r.status() >= 400 && r.status() !== 401 && problems.push(`HTTP ${r.status()}: ${r.url()}`));
  await signIn(page);
  await page.goto(`${base}/admin/Parcel`);
  const bar = page.getByRole("group", { name: "Date drill-down" });
  // The label and the count of each period button, as "label count".
  const periods = () => bar.locator("ul button").evaluateAll((els) => els.map((el) => [...el.childNodes].map((n) => n.textContent.trim()).filter(Boolean).join(" ")));
  const codes = async () => (await page.locator("tbody tr td:nth-child(2)").allTextContents()).map((t) => t.trim());
  const rowsAre = async (expected) => {
    for (let i = 0; i < 100; i++) {
      if (JSON.stringify(await codes()) === JSON.stringify(expected)) return;
      await page.waitForTimeout(100);
    }
    assert.deepEqual(await codes(), expected);
  };
  await rowsAre(["P-2024-03", "P-2025-03-a", "P-2025-03-b", "P-2025-04", "P-2026-01"]);

  // Years with their counts.
  await bar.getByRole("button", { name: "2024 1" }).waitFor();
  assert.deepEqual((await periods()), ["2024 1", "2025 3", "2026 1"]);

  await bar.getByRole("button", { name: "2025 3" }).click();
  await rowsAre(["P-2025-03-a", "P-2025-03-b", "P-2025-04"]);
  assert.ok(page.url().includes("f.placed%3Agte") || page.url().includes("f.placed:gte"), `the year writes the range filter: ${page.url()}`);
  assert.deepEqual((await periods()), ["March 2", "April 1"], "months of the year, without the empty ones");
  const chips = page.getByRole("list", { name: "Active filters" });
  assert.match(await chips.innerText(), /Placed/, "the range shows as a chip of the field");

  await bar.getByRole("button", { name: "March 2" }).click();
  await rowsAre(["P-2025-03-a", "P-2025-03-b"]);
  assert.deepEqual((await periods()), ["10 1", "20 1"], "days of the month");
  assert.equal(await bar.getByRole("navigation", { name: "Date breadcrumb" }).getByText("March").count(), 1, "the breadcrumb ends at the month");

  // The breadcrumb goes back, and the back button of the browser too.
  await bar.getByRole("button", { name: "All dates" }).click();
  await rowsAre(["P-2024-03", "P-2025-03-a", "P-2025-03-b", "P-2025-04", "P-2026-01"]);
  await chips.waitFor({ state: "detached" });
  await page.goBack();
  await rowsAre(["P-2025-03-a", "P-2025-03-b"]);
  assert.deepEqual(problems, []);
  await context.close();
});
