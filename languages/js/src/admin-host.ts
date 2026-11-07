/**
 * The host functions of the admin service: login, the user read at a session renewal, and password change through the auth app.
 *
 * Each function takes `(database, args)` and runs in `Database.bindRequest`, so it sees the transaction of
 * the request. The Rust service never hashes or compares a password. A host function must be async and
 * use the `database` it gets: a statement on another handle waits for a connection of its own.
 */
import { authenticate } from "./auth/auth.js";
import { activeModel } from "./auth/permissions.js";
import type { Database } from "./db.js";
import type { NativeHostDispatch } from "./native.js";

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
    user.setPassword(args.new_password);
    await user.update({ password: user.password });
    return { changed: true, password_hash: user.password };
  },
};

/** The one function that the native service calls for every host call. It answers an envelope: `{ok, value}` or `{ok: false, kind, message}`. */
export function hostDispatch(database: Database, host: Readonly<Record<string, HostFunction>> = HOST): NativeHostDispatch {
  return async (name, tx, payload, scope, tenant) => {
    try {
      const fn = host[name];
      if (fn === undefined) throw new Error(`no host function ${name}`);
      const args = JSON.parse(payload);
      const run = () => fn(database, args);
      const value =
        tx === null
          ? await run()
          : await database.bindRequest(tx, { scope: JSON.parse(scope), ...(tenant === null ? {} : { tenant: JSON.parse(tenant) }) }, run);
      return JSON.stringify({ ok: true, value: value ?? null });
    } catch (e) {
      const error = e instanceof Error ? e : new Error(String(e));
      return JSON.stringify({ ok: false, kind: error.name, message: error.message });
    }
  };
}
