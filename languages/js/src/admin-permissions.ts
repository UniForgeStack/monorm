/**
 * The permission names of an admin site in the auth tables: sync, stale names and seeded roles.
 * Both steps use the auth app through its API, so no SQL text lives here.
 */

import { activeModel as activeAuthModel, splitPerm, syncPermissions } from "./auth/permissions.js";
import type { Database } from "./db.js";
import type { Registry } from "./model.js";

/** What `Admin.syncPermissions` did. */
export interface SyncReport {
  /** Names that the call added to the auth tables. */
  readonly created: number;
  /** Names in the auth tables, under a label of this site, that the strategy no longer lists. Reported, never deleted. */
  readonly stale: readonly string[];
}

/** What `Admin.seedRoles` did. */
export interface RoleSeedReport {
  /** Names of the roles that the call created. */
  readonly rolesCreated: readonly string[];
  /** Role and permission pairs that the call added. */
  readonly grants: number;
}

/** `*` and `?` glob over `<label>.<codename>`. `[` and `]` are an error: Python `fnmatch` reads them as a class. */
function glob(pattern: string, whole: string, role: string): RegExp {
  if (/[[\]]/.test(pattern)) throw new Error(`role '${role}': the pattern '${whole}' has [ or ]; a glob here has only * and ?`);
  const escaped = pattern.replace(/[.+^${}()|\\]/g, "\\$&").replaceAll("*", ".*").replaceAll("?", ".");
  return new RegExp(`^${escaped}$`, "s");
}

/**
 * The names that `patterns` select, in the order of `names`.
 * A leading `-` removes what the patterns before it added. A pattern that matches no name is an error.
 */
export function resolvePatternGrants(patterns: readonly string[], names: readonly string[], role: string): string[] {
  const chosen = new Set<string>();
  for (const pattern of patterns) {
    const remove = pattern.startsWith("-");
    const re = glob(remove ? pattern.slice(1) : pattern, pattern, role);
    const hits = names.filter((n) => re.test(n));
    if (hits.length === 0) throw new Error(`role '${role}': the pattern '${pattern}' matches no permission name of the site`);
    for (const n of hits) {
      if (remove) chosen.delete(n);
      else chosen.add(n);
    }
  }
  return names.filter((n) => chosen.has(n));
}

function activeModel(name: string, registry: Registry): ReturnType<typeof activeAuthModel> {
  try {
    return activeAuthModel(name, registry);
  } catch {
    throw new Error(`the schema has no auth model ${name}: import the auth app in schema.prisma`);
  }
}

export async function sync(names: readonly string[], labels: readonly string[], database: Database, registry: Registry): Promise<SyncReport> {
  const Permission = activeModel("Permission", registry);
  const created = await syncPermissions(names, { using: database, registry });
  const rows = (await Permission.objects.using(database).filter(Permission.label.in([...labels]))) as { label: string; codename: string }[];
  const known = new Set(names);
  const stale = rows.map((r) => `${r.label}.${r.codename}`).filter((n) => !known.has(n)).sort();
  return { created, stale };
}

/** Creates each role that is absent and adds the pairs that are absent. It changes nothing on a second run and never deletes. */
export async function seed(
  roles: Readonly<Record<string, readonly string[]>>,
  names: readonly string[],
  database: Database,
  registry: Registry,
): Promise<RoleSeedReport> {
  const roleNames = Object.keys(roles);
  if (roleNames.length === 0) return { rolesCreated: [], grants: 0 };
  const wanted = new Map(roleNames.map((role) => [role, resolvePatternGrants(roles[role]!, names, role)]));
  const Role = activeModel("Role", registry);
  const Permission = activeModel("Permission", registry);
  const Through = activeModel("RolePermission", registry);
  const fetchRoles = async () =>
    new Map(((await Role.objects.using(database).filter(Role.name.in(roleNames))) as { id: unknown; name: string }[]).map((r) => [r.name, r]));
  let existing = await fetchRoles();
  const missing = roleNames.filter((n) => !existing.has(n));
  if (missing.length > 0) {
    await Role.objects.using(database).insertMany(missing.map((name) => ({ name }))).onConflict([Role.name], { update: false });
    existing = await fetchRoles();
  }
  const codes = new Set([...wanted.values()].flat());
  const labels = [...new Set([...codes].map((n) => splitPerm(n)[0]))].sort();
  const found = new Map(
    ((await Permission.objects.using(database).filter(Permission.label.in(labels))) as { id: unknown; label: string; codename: string }[]).map((p) => [`${p.label}.${p.codename}`, p]),
  );
  const absent = [...codes].filter((n) => !found.has(n)).sort();
  if (absent.length > 0) throw new Error(`the permissions [${absent.map((n) => `'${n}'`).join(", ")}] are not in the auth tables: run sync-permissions first`);
  const pairs = [...wanted].flatMap(([role, grants]) => grants.map((n) => ({ roleId: existing.get(role)!.id, permissionId: found.get(n)!.id })));
  let inserted = 0;
  if (pairs.length > 0) {
    inserted = (await Through.objects.using(database).insertMany(pairs).onConflict([Through.roleId, Through.permissionId], { update: false })) as number;
  }
  return { rolesCreated: missing, grants: inserted };
}
