/** `UserQueries`: the query set of every user model (`@@query_set("UserQueries")`). */

import { QuerySet, and, or, type Condition, type ModelSpec } from "@ufstack/monorm";

import { splitPerm } from "./permissions.js";

/**
 * The condition "this is an active user that has `perm`" (`"<label>.<codename>"`): superusers, and users
 * that hold it directly or through a role. An inactive user has no permission.
 */
export function permCondition(userModel: object, perm: string): Condition {
  const [label, codename] = splitPerm(perm);
  const user = userModel as Record<string, any>;
  const direct = user["permissions"];
  const viaRole = user["roles"].permissions;
  const has = or(
    and(direct.label.eq(label), direct.codename.eq(codename)),
    and(viaRole.label.eq(label), viaRole.codename.eq(codename)),
  );
  return and(user["isActive"].eq(true), or(user["isSuperuser"].eq(true), has)) as Condition;
}

export class UserQueries<M extends ModelSpec = ModelSpec> extends QuerySet<M> {
  /** The users that have `perm`. One query. */
  withPerm(perm: string): this {
    return this.filter(permCondition(this.meta.model, perm) as never) as unknown as this;
  }
}
