// Screenshots of the main admin pages in light and dark: `node shots.mjs <folder>`. It is a review aid, not a test, and its output is not committed.
// It starts the same project as the smoke test (harness.mjs), so it needs the `monorm` wheel in the active Python and the browser (`npm run install-browser`).
// It exits with 1 when a page logs a console error, a CSP violation or a failed request.
import { mkdirSync, rmSync } from "node:fs";
import { join, resolve } from "node:path";

import { chromium } from "playwright";

import { ZONE, startProject } from "./harness.mjs";

const out = process.argv[2];
if (!out) {
  console.error("usage: node shots.mjs <output folder>");
  process.exit(2);
}
const folder = resolve(out);
mkdirSync(folder, { recursive: true });

const { dir, server, base } = await startProject();
const browser = await chromium.launch();
const problems = [];
const written = [];
// A shot of an error state that the script itself causes: the console error and the HTTP status are expected.
let expected = false;

async function session(scheme, viewport = { width: 1280, height: 800 }) {
  const context = await browser.newContext({ timezoneId: ZONE, locale: "en-US", colorScheme: scheme, viewport });
  const page = await context.newPage();
  page.on("console", (m) => m.type() === "error" && !expected && !/status of (401|422)/.test(m.text()) && problems.push(`${scheme} console: ${m.text()}`));
  page.on("pageerror", (e) => problems.push(`${scheme} page error: ${e.message}`));
  page.on("response", (r) => r.status() >= 400 && r.status() !== 401 && r.status() !== 422 && !expected && problems.push(`${scheme} HTTP ${r.status()}: ${r.url()}`));
  return { context, page };
}

async function settle(page) {
  await page.waitForLoadState("networkidle");
  await page.waitForTimeout(300);
}

async function shot(page, scheme, name) {
  await settle(page);
  const file = join(folder, `${name}-${scheme}.png`);
  await page.screenshot({ path: file, fullPage: true });
  written.push(file);
}

// Without the wait for the network: for a state that lasts only while a request is open.
async function shotNow(page, scheme, name) {
  const file = join(folder, `${name}-${scheme}.png`);
  await page.screenshot({ path: file, fullPage: true });
  written.push(file);
}

async function signIn(page) {
  await page.goto(`${base}/admin/-/login`);
  await page.getByLabel("Username").waitFor();
}

try {
  for (const scheme of ["light", "dark"]) {
    const { context, page } = await session(scheme);
    await signIn(page);
    await shot(page, scheme, "01-login");
    await page.getByLabel("Username").fill("ann");
    await page.getByLabel("Password").fill("wrong");
    await page.getByRole("button", { name: "Sign in" }).click();
    await page.getByRole("alert").waitFor();
    await shot(page, scheme, "02-login-error");
    await page.getByLabel("Password").fill("pw-of-ann");
    await page.getByRole("button", { name: "Sign in" }).click();
    await page.getByRole("navigation", { name: "Resources" }).waitFor();

    // One audit row for the home page.
    if (scheme === "light") {
      await page.goto(`${base}/admin/Author/-/new`);
      await page.getByLabel("Name").fill("Grace");
      await page.getByRole("button", { name: "Save", exact: true }).click();
      await page.waitForURL(/\/admin\/Author\/\d+$/);
    }
    await page.goto(`${base}/admin/`);
    await page.getByRole("heading", { name: "Recent activity" }).waitFor();
    await shot(page, scheme, "03-home");

    const nav = page.getByRole("navigation", { name: "Resources" });
    await nav.getByRole("button", { name: /Users and access/ }).click();
    await page.getByRole("searchbox", { name: "Filter the resources" }).fill("r");
    await shot(page, scheme, "04-home-sidebar-open-group-and-filter");
    await page.getByRole("searchbox", { name: "Filter the resources" }).fill("");

    await page.getByRole("button", { name: "User menu" }).click();
    await page.getByRole("menu").waitFor();
    await shot(page, scheme, "05-user-menu");
    await page.keyboard.press("Escape");

    await page.goto(`${base}/admin/Book`);
    await page.getByText("2 records").waitFor();
    await shot(page, scheme, "06-list-book");
    await page.goto(`${base}/admin/Reviewer`);
    await page.getByText("25 records").waitFor();
    await shot(page, scheme, "07-list-reviewer-paged");
    await page.goto(`${base}/admin/Ticket`);
    await page.getByText("3 records").waitFor();
    await page.getByRole("checkbox").first().check();
    await shot(page, scheme, "08-list-ticket-selected");
    await page.getByRole("button", { name: "Close", exact: true }).first().click();
    await page.getByRole("alertdialog").waitFor();
    await shot(page, scheme, "09-confirm-bulk-action");
    await page.keyboard.press("Escape");

    await page.goto(`${base}/admin/Ticket`);
    await page.getByRole("button", { name: "Actions for row 1" }).click();
    await page.getByRole("menu").waitFor();
    await shot(page, scheme, "09b-list-row-menu");
    await page.keyboard.press("Escape");

    await page.goto(`${base}/admin/Book`);
    await page.getByText("2 records").waitFor();
    await page.getByRole("button", { name: /^Filters/ }).click();
    await page.getByText("Choose Author").click();
    await shot(page, scheme, "09c-list-filter-popover");
    await page.getByRole("button", { name: "Ada", exact: true }).click();
    await page.keyboard.press("Escape");
    await page.getByRole("list", { name: "Active filters" }).getByText("Author: Ada").waitFor();
    await shot(page, scheme, "09d-list-filter-chips");
    await page.goto(`${base}/admin/Book`);
    await page.getByText("2 records").waitFor();
    await page.getByRole("button", { name: "Columns" }).click();
    await page.getByRole("menu").waitFor();
    await shot(page, scheme, "09e-list-columns-menu");
    await page.keyboard.press("Escape");
    await page.getByRole("button", { name: "Compact" }).click();
    await shot(page, scheme, "09f-list-compact");
    await page.getByRole("button", { name: "Compact" }).click();
    await page.goto(`${base}/admin/Book?q=zzz-no-match`);
    await page.getByText("No records match").waitFor();
    await shot(page, scheme, "09g-list-empty");
    const slow = /\/api\/resources\/Book(\?|$)/;
    await page.route(slow, async (route) => {
      await new Promise((done) => setTimeout(done, 2500));
      await route.continue();
    });
    await page.goto(`${base}/admin/Book`, { waitUntil: "commit" });
    await page.getByRole("status", { name: "Loading the records" }).waitFor();
    await shotNow(page, scheme, "09h-list-loading");
    await page.getByText("2 records").waitFor();
    await page.unroute(slow);
    expected = true;
    await page.route(slow, (route) => route.fulfill({ status: 500, contentType: "application/json", body: JSON.stringify({ error: "The database did not answer." }) }));
    await page.goto(`${base}/admin/Book`);
    await page.getByRole("button", { name: "Try again" }).waitFor();
    await shot(page, scheme, "09i-list-error");
    await page.unroute(slow);
    expected = false;

    await page.goto(`${base}/admin/Book/1`);
    await page.getByText("First").first().waitFor();
    await shot(page, scheme, "10-detail-book");
    await page.getByRole("button", { name: "Delete" }).click();
    await page.getByRole("alertdialog").waitFor();
    await shot(page, scheme, "11-confirm-delete");
    await page.keyboard.press("Escape");
    await page.goto(`${base}/admin/Author/1?tab=books`);
    await page.getByRole("tab", { name: "Books" }).waitFor();
    await shot(page, scheme, "12-detail-tab-author-books");
    await page.goto(`${base}/admin/Movie/1`);
    await page.getByRole("tab", { name: "Summary" }).click();
    await page.getByText("Average of").waitFor();
    await shot(page, scheme, "13-package-tab-summary");
    await page.goto(`${base}/admin/Movie/1/edit`);
    await page.getByRole("group", { name: "Stars" }).waitFor();
    await shot(page, scheme, "14-package-widget-stars-form");

    await page.goto(`${base}/admin/Book/1/edit`);
    await page.getByRole("heading", { name: "Change Book" }).waitFor();
    await shot(page, scheme, "15-form-edit-book");
    await page.goto(`${base}/admin/Author/-/new`);
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await page.getByText("This field is required.").waitFor();
    await shot(page, scheme, "16-form-field-error");
    await page.goto(`${base}/admin/-/password`);
    await page.getByRole("heading", { name: /password/i }).waitFor();
    await shot(page, scheme, "17-password");

    await page.goto(`${base}/admin/`);
    await page.getByRole("button", { name: "Hide the sidebar" }).click();
    await shot(page, scheme, "18-home-sidebar-collapsed");
    await context.close();

    const narrow = await session(scheme, { width: 390, height: 800 });
    await signIn(narrow.page);
    await narrow.page.getByLabel("Username").fill("ann");
    await narrow.page.getByLabel("Password").fill("pw-of-ann");
    await narrow.page.getByRole("button", { name: "Sign in" }).click();
    await narrow.page.getByRole("heading", { name: "Recent activity" }).waitFor();
    await shot(narrow.page, scheme, "19-home-narrow");
    await narrow.page.goto(`${base}/admin/Book`);
    await narrow.page.getByText("2 records").waitFor();
    await shot(narrow.page, scheme, "20-list-narrow");
    await narrow.context.close();
  }
} finally {
  await browser.close();
  server.removeAllListeners("exit");
  server.kill("SIGTERM");
  rmSync(dir, { recursive: true, force: true });
}

console.log(`${written.length} screenshots in ${folder}`);
if (problems.length > 0) {
  console.error(problems.join("\n"));
  process.exit(1);
}
