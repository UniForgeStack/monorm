/** `UserMethods`: the methods of every user model (`@@model_methods("UserMethods")`). */

import { and, exists, or, outer, type BoundQuerySet, type Database, type ModelSpec, type Row } from "@ufstack/monorm";
import { boundMeta, databaseOf } from "@ufstack/monorm/ext";

import { checkPassword, makePassword, needsUpdate } from "./hashers.js";
import { activeModel } from "./permissions.js";
import { permCondition } from "./queries.js";

/** The columns the methods read; every user model has them. */
interface UserData {
  readonly id: unknown;
  readonly password: string;
  readonly isActive: boolean;
  readonly isSuperuser: boolean;
}

interface Bound {
  qs(): BoundQuerySet<ModelSpec>;
}

export class UserMethods {
  /** Hashes `raw` and sets the field on this snapshot. It does not write the row: `await user.update({ password: user.password })`. */
  setPassword(this: UserData, raw: string): void {
    (this as unknown as Row)["password"] = makePassword(raw);
  }

  /** Compares `raw` in constant time. With `update`, a correct `raw` and an old hash set a new hash (not written). */
  checkPassword(this: UserData & UserMethods, raw: string, options: { update?: boolean } = {}): boolean {
    const ok = checkPassword(raw, this.password);
    if (ok && options.update === true && needsUpdate(this.password)) this.setPassword(raw);
    return ok;
  }

  /** One query. An active superuser has every permission; an inactive user has none. */
  async hasPerm(this: UserData & Bound, perm: string): Promise<boolean> {
    const qs = this.qs();
    return qs.filter(permCondition(boundMeta(qs).model, perm) as never).exists();
  }

  /** One query for each name, in order; it stops at the first name that the user does not have. */
  async hasPerms(this: UserData & Bound & UserMethods, perms: Iterable<string>): Promise<boolean> {
    for (const perm of perms) if (!(await this.hasPerm(perm))) return false;
    return true;
  }

  /** `"<label>.<codename>"` of the user's own permissions and those of its roles. A superuser has all. */
  async getAllPermissions(this: UserData & Bound): Promise<Set<string>> {
    if (!this.isActive) return new Set();
    const { model, registry } = boundMeta(this.qs());
    const user = model as unknown as Record<string, any>;
    const Permission = activeModel("Permission", registry);
    let query = Permission.objects.using(databaseOf(this));
    if (!this.isSuperuser) {
      // the relations of `user` (`permissions`, `roles`) have the same names in every user model, unlike the join models
      const held = or(user["permissions"].id.eq(outer(Permission.id)), user["roles"].permissions.id.eq(outer(Permission.id)));
      query = query.filter(exists(user["objects"].filter(and(user["id"].eq(this.id), held))));
    }
    return new Set(((await query) as { label: string; codename: string }[]).map((p) => `${p.label}.${p.codename}`));
  }
}
