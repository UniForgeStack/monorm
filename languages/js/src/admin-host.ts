/**
 * The host functions of the admin service: login, the user read at a session renewal, and password change through the auth app.
 *
 * Each function takes `(database, args)` and runs in `Database.bindRequest`, so it sees the transaction of
 * the request. The Rust service never hashes or compares a password. A host function must be async and
 * use the `database` it gets: a statement on another handle waits for a connection of its own.
 */
import { randomBytes } from "node:crypto";
import { authenticate } from "./auth/auth.js";
import { makePassword } from "./auth/hashers.js";
import { loadRules, type PasswordRule, validatePassword } from "./auth/password.js";
import { activeModel } from "./auth/permissions.js";
import type { Database } from "./db.js";
import type { NativeHostDispatch } from "./native.js";

let loaded: { directory: string; rules: Promise<PasswordRule[]> } | undefined;

/** Loads `[auth] password_rules` once per process, so a wrong setting fails at start and a custom rule keeps its state. */
export function loadPasswordRules(): Promise<PasswordRule[]> {
  const directory = process.cwd();
  if (loaded?.directory === directory) return loaded.rules;
  const rules = loadRules(directory);
  loaded = { directory, rules };
  rules.catch(() => {
    if (loaded?.rules === rules) loaded = undefined; // a failed load is not kept
  });
  return rules;
}

export type HostFunction = (database: Database, args: any) => Promise<unknown>;

function userJson(user: Record<string, any>): Record<string, unknown> {
  // `password_hash` never reaches the cookie: the service keeps a keyed tag of it, so a changed password ends the session.
  return {
    password_hash: user["password"],
    key: typeof user["id"] === "bigint" ? Number(user["id"]) : user["id"],
    username: user["username"],
    display: user["username"],
    is_superuser: Boolean(user["isSuperuser"]),
    is_active: Boolean(user["isActive"]),
  };
}

export const HOST: Readonly<Record<string, HostFunction>> = {
  async "auth.authenticate"(database, args) {
    const user = await authenticate(args.username, args.password, { using: database, registry: database.registry });
    return user === null ? null : userJson(user);
  },
  async "auth.user"(database, args) {
    const User: Record<string, any> = activeModel("User", database.registry);
    const user = await User["objects"].using(database).filter(User["id"].eq(args.key)).first();
    return user === null || user === undefined ? null : userJson(user);
  },
  async "auth.change_password"(database, args) {
    const User: Record<string, any> = activeModel("User", database.registry);
    const user = await User["objects"].using(database).filter(User["id"].eq(args.key)).first();
    if (user === null || user === undefined || !user.isActive) return { changed: false, reason: "the user cannot change the password" };
    if (!user.checkPassword(args.old_password)) return { changed: false, reason: "the old password is wrong" };
    const errors = await validatePassword(args.new_password, user, { rules: await loadPasswordRules() });
    // The codes and params reach the browser and the audit row, so a rule must keep the password out of them.
    if (errors.length > 0) {
      return { changed: false, reason: errors.map((e) => e.message).join(" "), errors };
    }
    user.setPassword(args.new_password);
    await user.update({ password: user.password });
    return { changed: true, password_hash: user.password };
  },
  /**
   * The user of a verified single sign-on login: found by username, else created, and the role membership set to the roles of the claim.
   * An inactive user is returned as it is and nothing changes. A name in `roles` that no role has grants nothing.
   * A new user gets a random password that nobody knows, so only the single sign-on reaches the account.
   */
  async "auth.sso_login"(database, args) {
    const User: Record<string, any> = activeModel("User", database.registry);
    const Role: Record<string, any> = activeModel("Role", database.registry);
    const Join: Record<string, any> = activeModel(`${User["_meta"].name}Role`, database.registry);
    const users = User["objects"].using(database);
    const find = () => users.filter(User["username"].eq(args.username)).first();
    let user = await find();
    const created = user === null || user === undefined;
    if (created) {
      const row = { username: args.username, email: args.email ?? "", password: makePassword(randomBytes(32).toString("base64url")) };
      await users.insertMany([row]).onConflict([User["username"]], { update: false });
      user = await find();
    } else if (!user.isActive) {
      return { user: userJson(user), created: false };
    }
    const names = [...new Set<string>(args.roles ?? [])];
    const wanted = new Set<unknown>(
      names.length === 0 ? [] : ((await Role["objects"].using(database).filter(Role["name"].in(names))) as { id: unknown }[]).map((r) => r.id),
    );
    const held = Join["objects"].using(database).filter(Join["userId"].eq(user.id));
    const current = new Set<unknown>(((await held) as { roleId: unknown }[]).map((j) => j.roleId));
    const drop = [...current].filter((r) => !wanted.has(r));
    const add = [...wanted].filter((r) => !current.has(r));
    if (drop.length > 0) await held.filter(Join["roleId"].in(drop)).delete();
    if (add.length > 0) await Join["objects"].using(database).insertMany(add.map((roleId) => ({ userId: user.id, roleId })));
    return { user: userJson(user), created };
  },
};

/** The one function that the native service calls for every host call. It answers an envelope: `{ok, value}` or `{ok: false, kind, message}`. */
export function hostDispatch(database: Database, host: Readonly<Record<string, HostFunction>> = HOST): NativeHostDispatch {
  return async (name, tx, payload, scope) => {
    try {
      const fn = host[name];
      if (fn === undefined) throw new Error(`no host function ${name}`);
      const args = JSON.parse(payload);
      const run = () => fn(database, args);
      const value =
        tx === null
          ? await run()
          : await database.bindRequest(tx, { scope: JSON.parse(scope) }, run);
      return JSON.stringify({ ok: true, value: value ?? null });
    } catch (e) {
      const error = e instanceof Error ? e : new Error(String(e));
      return JSON.stringify({ ok: false, kind: error.name, message: error.message });
    }
  };
}
