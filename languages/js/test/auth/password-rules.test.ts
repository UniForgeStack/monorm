/** Password rules: the default list, the settings, custom rules. No database. The Python twin is `test_password_rules.py`. */

import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";

import { loadRules, passwordHelpTexts, rules, validatePassword, type PasswordRule } from "../../src/auth/index.js";

const CUSTOM = `
export class NoCompany {
  constructor(options) {
    this.word = options.word;
  }
  validate(password) {
    return password.toLowerCase().includes(this.word)
      ? [{ code: "password_has_company", message: "The password has " + this.word + ".", params: { word: this.word } }]
      : [];
  }
  helpText() {
    return "Your password can't have " + this.word + ".";
  }
}
export class Later extends NoCompany {
  async validate(password) {
    return super.validate(password);
  }
}
`;

const START = process.cwd();
const DIRS: string[] = [];
after(() => {
  process.chdir(START);
  for (const dir of DIRS) rmSync(dir, { recursive: true, force: true });
});

/** A project directory with `monorm.toml` and the custom rules in `rules_mod.mjs`; it becomes the current directory. */
function project(settings = ""): string {
  const dir = mkdtempSync(join(tmpdir(), "monorm-rules-"));
  DIRS.push(dir);
  writeFileSync(join(dir, "monorm.toml"), settings);
  writeFileSync(join(dir, "rules_mod.mjs"), CUSTOM);
  process.chdir(dir);
  return dir;
}

async function codes(password: string, user: unknown = null, options: { rules?: PasswordRule[] } = {}): Promise<string[]> {
  return (await validatePassword(password, user, options)).map((e) => e.code);
}

const ANNA = { username: "annabelle", email: "ann@example.com" };

test("numeric means decimal digits of any script and not other numerals", async () => {
  project();
  assert.deepEqual(await codes("٣٤٥٦٧٨٩٠١", ANNA), ["password_entirely_numeric"]);
  assert.deepEqual(await codes("½½½½½½½½", ANNA), []);
});

test("the default rules refuse a short, a common, a numeric and a similar password", async () => {
  project();
  assert.deepEqual(await codes("x7!Qr", ANNA), ["password_too_short"]);
  assert.deepEqual(await codes("password", ANNA), ["password_too_common"]);
  assert.deepEqual(await codes("PassWord ", ANNA), ["password_too_common"]);
  assert.deepEqual(await codes("20250101", ANNA), ["password_entirely_numeric"]);
  assert.deepEqual(await codes("annabelle1", ANNA), ["password_too_similar"]);
  assert.deepEqual(await codes("correct horse battery", ANNA), []);
  assert.deepEqual(await codes("1234", ANNA), ["password_too_short", "password_too_common", "password_entirely_numeric"]);
});

test("the length counts code points after NFC", async () => {
  const four = [rules.minLength({ min: 4 })];
  assert.deepEqual(await codes("ééé", null, { rules: four }), ["password_too_short"]);
  assert.deepEqual(await codes("éééé", null, { rules: four }), []);
  assert.deepEqual(await codes("😀😀😀", null, { rules: four }), ["password_too_short"]);
  assert.deepEqual(await codes("😀😀😀😀", null, { rules: four }), []);
});

test("a non-ASCII user name is compared after NFC and lower case", async () => {
  const similar = [rules.userSimilarity()];
  assert.deepEqual(await codes("ÉMILIE2025", { username: "émilie" }, { rules: similar }), ["password_too_similar"]);
  assert.deepEqual(await codes("Zürich-Ωmega-77", { username: "émilie" }, { rules: similar }), []);
});

test("the error has a code, a message and params", async () => {
  const errors = await validatePassword("short", null, { rules: [rules.minLength({ min: 12 })] });
  assert.deepEqual(errors, [
    { code: "password_too_short", message: "This password is too short. It must contain at least 12 characters.", params: { min: 12 } },
  ]);
  const [similar] = await validatePassword("annabelle1", ANNA, { rules: [rules.userSimilarity()] });
  assert.deepEqual(similar!.params, { attribute: "username" });
});

test("the user is a model, a dict or none", async () => {
  const similar = [rules.userSimilarity({ attributes: ["username", "first_name"] })];
  assert.deepEqual(await codes("mohammed1", { username: "x", firstName: "Mohammed" }, { rules: similar }), ["password_too_similar"]);
  assert.deepEqual(await codes("mohammed1", { first_name: "Mohammed" }, { rules: similar }), ["password_too_similar"]);
  assert.deepEqual(await codes("mohammed1", null, { rules: similar }), []);
  assert.deepEqual(await codes("mohammed1", { username: null }, { rules: similar }), []);
});

test("a list given in code replaces the settings", async () => {
  project("[auth]\npassword_rules = [{rule = 'min_length', min = 30}]\n");
  assert.deepEqual(await codes("password-long-enough"), ["password_too_short"]);
  assert.deepEqual(await codes("password-long-enough", null, { rules: [rules.common()] }), []);
});

test("an empty list turns the rules off", async () => {
  project("[auth]\npassword_rules = []\n");
  assert.deepEqual(await codes("1"), []);
  assert.deepEqual(await passwordHelpTexts(), []);
});

test("a custom rule from the settings runs in list order", async () => {
  project(
    "[auth]\npassword_rules = [{rule = './rules_mod.mjs:NoCompany', word = 'acme'}, {rule = 'min_length', min = 12}, " +
      "{rule = './rules_mod.mjs:Later', word = 'zed'}]\n",
  );
  assert.deepEqual(await codes("Acme-zed"), ["password_has_company", "password_too_short", "password_has_company"]);
  assert.deepEqual(await codes("a quiet sunny afternoon"), []);
  assert.deepEqual(await passwordHelpTexts(), [
    "Your password can't have acme.",
    "Your password must contain at least 12 characters.",
    "Your password can't have zed.",
  ]);
});

test("a wrong setting is an error when the rules load", async () => {
  const dir = project("[auth]\npassword_rules = [{rule = 'min_length', mn = 3}]\n");
  await assert.rejects(loadRules(), /monorm\.toml: `auth\.password_rules\[0\]\.mn` is not an option of `min_length`/);
  writeFileSync(join(dir, "monorm.toml"), "[auth]\npassword_rules = [{rule = 'lenght'}]\n");
  await assert.rejects(validatePassword("x"), /`auth\.password_rules\[0\]\.rule` `lenght` is not a rule/);
});

test("a custom rule that cannot be imported is an error when the rules load", async () => {
  project("[auth]\npassword_rules = [{rule = './rules_mod.mjs:Missing'}]\n");
  await assert.rejects(loadRules(), /password rule `\.\/rules_mod\.mjs:Missing`/);
});

test("the help texts follow the list", async () => {
  project();
  assert.equal((await passwordHelpTexts())[0], "Your password must contain at least 8 characters.");
  assert.equal((await passwordHelpTexts()).length, 4);
  assert.deepEqual(await passwordHelpTexts({ rules: [rules.numeric()] }), ["Your password can't be entirely numeric."]);
});

test("a common file replaces the bundled list", async () => {
  const dir = project();
  const listing = join(dir, "list.txt");
  writeFileSync(listing, "Hunter2hunter2\n");
  const only = [rules.common({ path: listing })];
  assert.deepEqual(await codes("HUNTER2hunter2", null, { rules: only }), ["password_too_common"]);
  assert.deepEqual(await codes("password", null, { rules: only }), []);
});

test("a wrong option in code is an error when the rule is made", () => {
  assert.throws(() => rules.minLength({ min: 0 }), /min/);
  assert.throws(() => rules.userSimilarity({ maxSimilarity: 2 }), /max_similarity/);
});
