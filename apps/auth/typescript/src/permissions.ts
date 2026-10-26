/** Permission names (`"blog.add_post"`) and `syncPermissions()`. */

import { registry as defaultRegistry, type Database, type ModelClass, type ModelSpec, type Registry } from "orm";
import { behaviorValue } from "orm/ext";

export const ACTIONS = ["add", "change", "delete", "view"] as const;

export type AnyModel = ModelClass<ModelSpec> & Record<string, any>;

/** `"blog.add_post"` is the label `blog` and the codename `add_post`. */
export function splitPerm(perm: string): [label: string, codename: string] {
  const dot = perm.indexOf(".");
  if (dot < 1 || dot === perm.length - 1) throw new TypeError(`permission ${JSON.stringify(perm)} must have the form '<label>.<codename>'`);
  return [perm.slice(0, dot), perm.slice(dot + 1)];
}

/**
 * The model class for `name`: the model that overrides it (`@@inherit.override`), else `name` itself.
 * The app does not depend on the `@orm/inherit` package, so it reads `model_overrides` itself.
 *
 * @internal
 */
export function activeModel(name: string, registry: Registry = defaultRegistry): AnyModel {
  const overrides = (behaviorValue("model_overrides", registry) ?? {}) as Readonly<Record<string, string>>;
  return registry.get(Object.hasOwn(overrides, name) ? overrides[name]! : name).model as AnyModel;
}

/**
 * Creates `add`, `change`, `delete` and `view` for each model in one bulk insert; rows that exist stay.
 * The permission names are `<label>.<action>_<model name in lower case>`. Returns the number of new rows.
 */
export async function syncPermissions(
  models: readonly ModelClass<ModelSpec>[],
  label: string,
  options: { using?: Database; registry?: Registry } = {},
): Promise<number> {
  const Permission = activeModel("Permission", options.registry);
  const rows = models.flatMap((m) =>
    ACTIONS.map((action) => ({ label, codename: `${action}_${m._meta.name.toLowerCase()}`, name: `Can ${action} ${m._meta.name.toLowerCase()}` })),
  );
  if (rows.length === 0) return 0;
  const query = Permission.objects.using(options.using).insertMany(rows);
  return (await query.onConflict([Permission.label, Permission.codename], { update: false })) as number;
}
