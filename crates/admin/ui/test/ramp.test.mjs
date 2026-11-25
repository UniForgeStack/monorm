// Unit tests of the shell rules (src/lib/ramp.ts): the colour ramp, the environment level, the favicon and the initials.
import assert from "node:assert/strict";
import { test } from "node:test";

import { STEPS, envLevel, faviconUrl, hueOf, initials, ramp } from "../src/lib/ramp.ts";

test("the hue of a colour is its OKLCH hue", () => {
  assert.ok(Math.abs(hueOf("#ff0000") - 29.2) < 0.5);
  assert.ok(Math.abs(hueOf("#0000ff") - 264.1) < 0.5);
  assert.equal(hueOf("#f00"), hueOf("#ff0000"));
  assert.equal(hueOf("#808080"), 0);
});

test("the ramp has the 11 steps with the fixed lightness and chroma, and only the hue follows the colour", () => {
  const r = ramp("#e11d48");
  assert.equal(Object.keys(r).length, 11);
  const hue = hueOf("#e11d48");
  assert.equal(r["--primary-500"], `oklch(62.7% 0.265 ${hue})`);
  assert.equal(r["--primary-600"], `oklch(55.8% 0.288 ${hue})`);
  assert.deepEqual(Object.keys(r), Object.keys(STEPS).map((s) => `--primary-${s}`));
});

test("the level of the chip: the set level wins, else the label decides, case ignored", () => {
  assert.equal(envLevel("PRODUCTION"), "danger");
  assert.equal(envLevel("prod"), "danger");
  assert.equal(envLevel("Staging"), "warning");
  assert.equal(envLevel("local"), "info");
  assert.equal(envLevel("production", "info"), "info");
  assert.equal(envLevel("local", "danger"), "danger");
  assert.equal(envLevel("local", "bogus"), "info");
});

test("the favicon is an SVG data URL with the three ramp steps", () => {
  const url = decodeURIComponent(faviconUrl("#a855f7"));
  assert.ok(url.startsWith("data:image/svg+xml,<svg"));
  for (const l of ["71.4%", "62.7%", "55.8%"]) assert.ok(url.includes(`oklch(${l}`));
});

test("initials", () => {
  assert.equal(initials("Ann Lee"), "AL");
  assert.equal(initials("ann@example.com"), "AE");
  assert.equal(initials("ann"), "AN");
  assert.equal(initials(""), "?");
});
