/** `authenticate()`. */

import type { Database, Registry } from "@ufstack/monorm";

import { checkPassword, makePassword } from "./hashers.js";
import { activeModel, type AnyModel } from "./permissions.js";

// Checked when the user does not exist, so the time does not show whether a username exists.
const dummy = makePassword("@ufstack/monorm-auth dummy password");

/**
 * The active user with this username and password, else `null`. An old hash is replaced by an argon2id hash.
 *
 * `model` is the user model. The default is the model that overrides `User`, else `User`; an `@@inherit.extends` child needs it.
 */
export async function authenticate(
  username: string,
  password: string,
  options: { using?: Database; registry?: Registry; model?: AnyModel } = {},
): Promise<any> {
  const User = options.model ?? activeModel("User", options.registry);
  const user: Record<string, any> | null = await User.objects.using(options.using).filter(User.username.eq(username)).first();
  if (user === null || user === undefined) {
    checkPassword(password, dummy);
    return null;
  }
  const old = user.password as string;
  if (!user.checkPassword(password, { update: true }) || !user.isActive) return null;
  if (user.password !== old) await user.update({ password: user.password });
  return user;
}
