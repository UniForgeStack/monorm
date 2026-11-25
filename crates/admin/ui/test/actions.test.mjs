// Unit tests of the header grouping rule (src/lib/actions.ts): buttons, named menus and "More".
import assert from "node:assert/strict";
import { test } from "node:test";

import { MAX_BUTTONS, groupActions } from "../src/lib/actions.ts";

const names = (list) => list.map((a) => a.name);

test("an action with no menu is a button", () => {
  const g = groupActions([{ name: "a" }, { name: "b", menu: null }]);
  assert.deepEqual(names(g.buttons), ["a", "b"]);
  assert.deepEqual(g.menus, []);
  assert.deepEqual(g.more, []);
});

test("actions with the same menu form one dropdown, in the order of the first action", () => {
  const g = groupActions([
    { name: "a", menu: "Export" },
    { name: "b", menu: "Share" },
    { name: "c", menu: "Export" },
  ]);
  assert.deepEqual(g.buttons, []);
  assert.deepEqual(g.menus.map((m) => m.label), ["Export", "Share"]);
  assert.deepEqual(names(g.menus[0].items), ["a", "c"]);
});

test("the buttons over the limit go to More", () => {
  const all = ["a", "b", "c", "d", "e"].map((name) => ({ name }));
  const g = groupActions(all);
  assert.equal(MAX_BUTTONS, 3);
  assert.deepEqual(names(g.buttons), ["a", "b", "c"]);
  assert.deepEqual(names(g.more), ["d", "e"]);
});

test("menu actions do not count against the button limit", () => {
  const g = groupActions([{ name: "m", menu: "X" }, { name: "a" }, { name: "b" }, { name: "c" }]);
  assert.deepEqual(names(g.buttons), ["a", "b", "c"]);
  assert.deepEqual(g.more, []);
});

test("no actions give empty groups", () => {
  assert.deepEqual(groupActions([]), { buttons: [], menus: [], more: [] });
});
