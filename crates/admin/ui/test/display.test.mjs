// Unit tests of the pure display rules of the SPA (src/display.ts). Run: npm test (Node 24 or later strips the types).
import assert from "node:assert/strict";
import { test } from "node:test";

import { BADGE_COLOURS, shown } from "../src/display.ts";

const bool = { widget: "checkbox", array: false };

test("a bool in a list is a check, a cross or a dash, each with a label", () => {
  assert.deepEqual(shown(true, bool, true), { kind: "icon", icon: "check", label: "Yes" });
  assert.deepEqual(shown(false, bool, true), { kind: "icon", icon: "x", label: "No" });
  assert.deepEqual(shown(null, bool, true), { kind: "icon", icon: "dash", label: "Empty" });
});

test("the detail page and a column that is not a bool draw no icon", () => {
  assert.equal(shown(true, bool, false), null);
  assert.equal(shown(1, { widget: "number", array: false }, true), null);
  assert.equal(shown([true], { widget: "checkbox", array: true }, true), null);
});

test("a badge has the colour of its value, one class per colour", () => {
  const badge = Object.fromEntries(BADGE_COLOURS.map((c) => [c, c]));
  for (const colour of BADGE_COLOURS) {
    assert.deepEqual(shown(colour, { widget: "text", array: false, badge }, true), { kind: "badge", colour, text: colour });
  }
});

test("a value outside the map is grey, a null value has no badge", () => {
  const column = { widget: "text", array: false, badge: { open: "green" } };
  assert.deepEqual(shown("other", column, true), { kind: "badge", colour: "grey", text: "other" });
  assert.equal(shown(null, column, true), null);
  assert.deepEqual(shown("open", { ...column, badge: { open: "pink" } }, true), { kind: "badge", colour: "grey", text: "open" });
});

test("an int has a text key and a bool badge replaces the icon", () => {
  assert.deepEqual(shown(3, { widget: "number", array: false, badge: { 3: "red" } }, true), { kind: "badge", colour: "red", text: "3" });
  const column = { ...bool, badge: { true: "green", false: "grey" } };
  assert.deepEqual(shown(true, column, true), { kind: "badge", colour: "green", text: "Yes" });
  assert.deepEqual(shown(false, column, false), { kind: "badge", colour: "grey", text: "No" });
});
