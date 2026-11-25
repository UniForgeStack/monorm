import { getDatabase, unscoped } from "monorm";
import type { BoundQuerySet, Database, ModelSpec, QuerySet, WriteStatement } from "monorm";
import { behaviorField, behaviorRecords, behaviorValue, boundMeta, boundWrite, hardDeleteStatement } from "monorm/ext";
import type { BoundQuerySetCtor, QuerySetCtor } from "monorm/ext";

type AnyQuerySet = QuerySet<ModelSpec>;
type Deleted = ReturnType<typeof behaviorField>;

const DEFAULT = "deleted" as never;
type WriteOf<T> = T extends QuerySet<infer M, any, any, any, any, any> ? WriteStatement<M> : never;

const SOURCE = "monorm.soft-delete";

// One lookup for each model class, not one for each call.
const fields = new WeakMap<object, Deleted>();

function deletedAt(qs: { readonly meta: AnyQuerySet["meta"] }): Deleted {
  const meta = qs.meta;
  let field = fields.get(meta);
  if (field === undefined) {
    const record = behaviorRecords(qs as AnyQuerySet, "delete_rewrites").find((r) => r["source"] === SOURCE);
    if (record === undefined) throw new TypeError(`${meta.name} has no @@soft_delete.deleted_at attribute`);
    field = behaviorField(meta, record);
    fields.set(meta, field);
  }
  return field;
}

export function softDeleteQuerySet<B extends QuerySetCtor>(Base: B) {
  return class SoftDelete extends Base {
    /**
     * `DELETE` every matching row also for a model with `@@soft_delete.deleted_at`. In
     * database mode, the trigger soft-deletes a live row, so this deletes only rows that are
     * already soft-deleted. With `hard_delete_setting` in the datasource the statement sets that
     * setting, and a live row is deleted too.
     */
    hardDelete<T extends AnyQuerySet>(this: T): WriteOf<T> {
      deletedAt(this);
      return hardDeleteStatement(this.withoutDefaults(DEFAULT)) as never;
    }

    /** Clear the soft-delete field of the matching soft-deleted rows. It bypasses the `"deleted"` default only. */
    undelete<T extends AnyQuerySet>(this: T): WriteOf<T> {
      const column = deletedAt(this);
      return this.withoutDefaults(DEFAULT).filter(column.column.isNotNull() as never).update({ [column.name]: null } as never) as never;
    }

    /** The rows with the soft-deleted ones. The other defaults, such as a tenant filter, stay. */
    allWithDeleted<T extends AnyQuerySet>(this: T): T {
      deletedAt(this);
      return this.withoutDefaults(DEFAULT) as T;
    }

    /** Only the soft-deleted rows; the other defaults stay. */
    deletedOnly<T extends AnyQuerySet>(this: T): T {
      return this.withoutDefaults(DEFAULT).filter(deletedAt(this).column.isNotNull() as never) as unknown as T;
    }
  };
}

type AnyBound = BoundQuerySet<ModelSpec, unknown>;
type ObjectsOf<T> = T extends BoundQuerySet<any, infer R> ? R[] : never;

/** The soft-delete members that are safe on a bound query set: the result is the held objects. */
export function softDeleteBoundQuerySet<B extends BoundQuerySetCtor>(Base: B) {
  return class SoftDeleteBound extends Base {
    /** `DELETE` the rows of the objects also for a soft-delete model. Gives the objects whose row was deleted. */
    hardDelete<T extends AnyBound>(this: T): Promise<ObjectsOf<T>> {
      deletedAt({ meta: boundMeta(this) });
      return boundWrite(this.withoutDefaults(DEFAULT), (qs) => hardDeleteStatement(qs)) as never;
    }

    /** Clear the soft-delete field of the soft-deleted rows of the objects, filling the objects. */
    undelete<T extends AnyBound>(this: T): Promise<ObjectsOf<T>> {
      const column = deletedAt({ meta: boundMeta(this) });
      return boundWrite(this.withoutDefaults(DEFAULT), (qs) => qs.filter(column.column.isNotNull() as never).update({ [column.name]: null } as never)) as never;
    }

    /** The objects' rows with the soft-deleted ones. The other defaults stay. */
    allWithDeleted<T extends AnyBound>(this: T): T {
      deletedAt({ meta: boundMeta(this) });
      return this.withoutDefaults(DEFAULT) as T;
    }

    /** Only the soft-deleted rows of the objects. */
    deletedOnly<T extends AnyBound>(this: T): T {
      return this.withoutDefaults(DEFAULT).filter(deletedAt({ meta: boundMeta(this) }).column.isNotNull() as never) as T;
    }
  };
}

type InstanceLike = { readonly pk: unknown; qs(): { withoutDefaults(name: never): { hardDelete(): Promise<unknown> } } };

export class SoftDeleteInstance {
  /** `DELETE ... WHERE pk = ...` also for a soft-delete model; in database mode, the trigger soft-deletes a live row, unless the datasource has `hard_delete_setting`. */
  async hardDelete(this: InstanceLike): Promise<void> {
    // A held instance applies no scope filter: its primary key names the row.
    await this.qs().withoutDefaults(DEFAULT).hardDelete();
  }

  /** Clear the soft-delete field of this row and fill it from the updated row. */
  async undelete(this: InstanceLike): Promise<void> {
    const column = deletedAt({ meta: (this as unknown as { constructor: { meta: AnyQuerySet["meta"] } }).constructor.meta });
    await (this as any).update({ [column.name]: null });
  }
}

type Row = Record<string, unknown>;
type Owned = { model: string; key: string[]; passes: number; report_sql: string; repair_sql: string; role_sql: string | null };

export interface SweepOwnedOptions {
  /** Stamp the rows that no live owner holds, until a pass finds none. Default `false`: only report them. */
  repair?: boolean;
  /** Check only these target models (classes or names). */
  models?: readonly (string | { readonly _meta: { readonly name: string } })[];
  db?: Database;
}

function ownedRecords(db: Database, models: SweepOwnedOptions["models"]): Owned[] {
  const records = ((behaviorValue("extension_records", db.registry) as Row[] | undefined) ?? []).filter((r) => r["source"] === SOURCE && r["kind"] === "owned");
  const all = records.map((r) => ({ ...(r["data"] as Omit<Owned, "model">), model: String(r["model"]) }));
  if (models === undefined) return all;
  const names = models.map((m) => (typeof m === "string" ? m : m._meta.name));
  const unknown = names.filter((n) => !all.some((o) => o.model === n));
  if (unknown.length > 0) throw new TypeError(`sweepOwned() has no model ${unknown.join(", ")}: no owning relation points at it`);
  return all.filter((o) => names.includes(o.model));
}

/**
 * Find the live rows that every owner has left: all their owners are soft-deleted, and at least
 * one owner was. Returns `{ model name: [primary key columns of each row] }`; a model with no such row
 * is absent, so `{}` is a clean database. With `repair: true`, stamps each row with the current time,
 * as the sweep trigger does, and repeats until a pass finds none. It throws when it needs more passes than the
 * number of owning relations plus one; the repair runs in one transaction, so the error rolls back all passes. It runs without the scope filters. On PostgreSQL it refuses
 * a role that is neither a superuser nor in `bypass_roles` before it reads or writes anything. It
 * takes no lock: run it in a quiet window, or run it again.
 */
export async function sweepOwned(options: SweepOwnedOptions = {}): Promise<Record<string, Row[]>> {
  const db = options.db ?? getDatabase();
  const records = ownedRecords(db, options.models);
  const keys = [...new Set(((behaviorValue("scoped_fields", db.registry) as Row[] | undefined) ?? []).map((r) => String(r["key"])))].sort();
  const found: Record<string, Row[]> = {};
  const run = async (): Promise<void> => {
    const sql = records.find((r) => r.role_sql)?.role_sql;
    if (sql && !(await db.fetch(sql))[0]?.["ok"]) throw new Error("sweepOwned() needs a superuser or a role of bypass_roles");
    if (options.repair !== true) {
      for (const r of records) {
        const rows = await db.fetch(r.report_sql);
        if (rows.length > 0) found[r.model] = rows;
      }
      return;
    }
    await db.transaction(async () => {
      for (let pass = 0; pass < Math.max(1, ...records.map((r) => r.passes)); pass++) {
        let changed = false;
        for (const r of records) {
          const rows = await db.fetch(r.repair_sql);
          if (rows.length > 0) (found[r.model] ??= []).push(...rows);
          changed ||= rows.length > 0;
        }
        if (!changed) return;
      }
      throw new Error("sweepOwned({ repair: true }) did not settle: the owner rows keep changing, or the owning relations form a loop of more than one pass");
    });
  };
  if (keys.length > 0) await unscoped(keys, run);
  else await run();
  return found;
}
