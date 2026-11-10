/**
 * Password rules: `validatePassword` runs the rules of `[auth] password_rules` (`passwordRules` in `package.json`),
 * or the rules that the caller gives. The built-in rules run the Rust check, the same code as the Python twin.
 *
 * A rule has `validate(password, user)`, sync or async, which returns a list of `PasswordError` (it does not throw
 * for a weak password), and `helpText()`. A custom rule is `module:attr` in the settings: a class or a function
 * that gets the other keys of its entry as one options object.
 */

import { loadSpec } from "../load-spec.js";
import { native } from "../native.js";

/** One broken rule. `code` is stable: a project translates `message` by `code` and `params`. `params` reach the browser and the audit row: never put the password in them. */
export interface PasswordError {
  readonly code: string;
  readonly message: string;
  readonly params: Readonly<Record<string, unknown>>;
}

export interface PasswordRule {
  validate(password: string, user: unknown): PasswordError[] | Promise<PasswordError[]>;
  helpText(): string;
}

type Entry = Record<string, unknown>;

function camel(name: string): string {
  return name.replace(/_([a-z])/g, (_, c: string) => c.toUpperCase());
}

/** The attribute `name` of a user model or object (`first_name` or `firstName`); `undefined` when there is none. */
function userAttribute(user: unknown, name: string): unknown {
  if (user === null || user === undefined) return undefined;
  const record = user as Record<string, unknown>;
  return record[name] ?? record[camel(name)];
}

function builtIn(entry: Entry, attributes: readonly string[] = []): PasswordRule {
  const own = JSON.stringify([entry]);
  const rule: PasswordRule = {
    validate(password, user) {
      const attrs: Record<string, string> = {};
      for (const name of attributes) {
        const value = userAttribute(user, name);
        if (typeof value === "string") attrs[name] = value;
      }
      return JSON.parse(native().passwordCheck(own, password, JSON.stringify(attrs))) as PasswordError[];
    },
    helpText: () => (JSON.parse(native().passwordHelp(own)) as string[])[0]!,
  };
  rule.helpText(); // the options are checked when the rule is made
  return rule;
}

/** The built-in rules. */
export const rules = {
  /** At least `min` characters (default 8), counted as code points after NFC. */
  minLength: (options: { min?: number } = {}): PasswordRule => builtIn({ rule: "min_length", min: options.min ?? 8 }),
  /** Not on the list of common passwords: the bundled list, or the file `path` (one password for each line). */
  common: (options: { path?: string } = {}): PasswordRule =>
    builtIn(options.path === undefined ? { rule: "common" } : { rule: "common", path: options.path }),
  /** Not only digits. */
  numeric: (): PasswordRule => builtIn({ rule: "numeric" }),
  /** Not too close to a user attribute: the character overlap, at least `maxSimilarity` (Django's quick ratio). */
  userSimilarity: (options: { attributes?: readonly string[]; maxSimilarity?: number } = {}): PasswordRule => {
    const attributes = options.attributes ?? ["username", "email"];
    return builtIn({ rule: "user_similarity", attributes, max_similarity: options.maxSimilarity ?? 0.7 }, attributes);
  },
};

async function custom(spec: string, options: Entry): Promise<PasswordRule> {
  const target = await loadSpec(spec, "password rule");
  let rule: unknown;
  try {
    rule =
      typeof target === "function"
        ? /^\s*class\b/.test(Function.prototype.toString.call(target))
          ? new (target as new (o: Entry) => unknown)(options)
          : (target as (o: Entry) => unknown)(options)
        : target;
  } catch (e) {
    throw new Error(`password rule \`${spec}\`: ${e instanceof Error ? e.message : String(e)}`);
  }
  const r = rule as Partial<PasswordRule> | null;
  if (typeof r?.validate !== "function" || typeof r.helpText !== "function") {
    throw new Error(`password rule \`${spec}\` has no validate() and helpText()`);
  }
  return r as PasswordRule;
}

/**
 * The rules of `[auth] password_rules` in the project settings of `directory` (default: the current directory).
 * No setting gives the default rules; an empty list gives no rules. A wrong setting, or a custom rule that
 * cannot be imported, throws here and not at the first password.
 */
export async function loadRules(directory: string = process.cwd()): Promise<PasswordRule[]> {
  const entries = JSON.parse(native().passwordRules(directory)) as Entry[];
  const out: PasswordRule[] = [];
  for (const entry of entries) {
    const { rule: name, ...options } = entry as Entry & { rule: string };
    if (name.includes(":")) {
      out.push(await custom(name, options));
      continue;
    }
    switch (name) {
      case "min_length":
        out.push(rules.minLength(options as { min?: number }));
        break;
      case "common":
        out.push(rules.common(options as { path?: string }));
        break;
      case "numeric":
        out.push(rules.numeric());
        break;
      default:
        out.push(
          rules.userSimilarity({
            attributes: options["attributes"] as string[],
            maxSimilarity: options["max_similarity"] as number,
          }),
        );
    }
  }
  return out;
}

/**
 * Runs every rule in list order and returns all errors; an empty list means the password is good.
 * `user` is a user model, an object or `null` (for example before the row exists). `createUser` and
 * `setPassword` do not call this: forms and commands do.
 */
export async function validatePassword(
  password: string,
  user: unknown = null,
  options: { rules?: readonly PasswordRule[] } = {},
): Promise<PasswordError[]> {
  const errors: PasswordError[] = [];
  for (const rule of options.rules ?? (await loadRules())) errors.push(...(await rule.validate(password, user)));
  return errors;
}

/** The help text of each rule, in list order, for a form. */
export async function passwordHelpTexts(options: { rules?: readonly PasswordRule[] } = {}): Promise<string[]> {
  return (options.rules ?? (await loadRules())).map((rule) => rule.helpText());
}
