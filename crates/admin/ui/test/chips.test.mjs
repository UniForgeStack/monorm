// The text of each environment chip meets WCAG AA (4.5:1) on its soft fill, in both themes, from the tokens in styles.css.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const css = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8");
const block = (head) => css.slice(css.indexOf(`${head} {`), css.indexOf("\n}", css.indexOf(`${head} {`)));
const token = (scope, name) => scope.match(new RegExp(`--${name}: (#[0-9a-f]{6});`))?.[1];
const channel = (v) => (v / 255 <= 0.03928 ? v / 255 / 12.92 : ((v / 255 + 0.055) / 1.055) ** 2.4);
const luminance = (hex) => [1, 3, 5].map((i) => channel(parseInt(hex.slice(i, i + 2), 16))).reduce((a, v, i) => a + v * [0.2126, 0.7152, 0.0722][i], 0);
const contrast = (a, b) => (Math.max(luminance(a), luminance(b)) + 0.05) / (Math.min(luminance(a), luminance(b)) + 0.05);

for (const [theme, scope] of [["light", block(":root")], ["dark", block(".dark")]]) {
  for (const [text, fill] of [["info", "info-soft"], ["warning", "warning-soft"], ["destructive", "danger-soft"]]) {
    test(`the ${text} chip reads at 4.5:1 (${theme})`, () => {
      const [fg, bg] = [token(scope, text), token(scope, fill)];
      assert.ok(fg && bg, `${text} and ${fill} are set`);
      assert.ok(contrast(fg, bg) >= 4.5, `${fg} on ${bg} is ${contrast(fg, bg).toFixed(2)}:1`);
    });
  }
}
