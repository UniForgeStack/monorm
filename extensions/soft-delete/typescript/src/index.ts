import type { InspectedStatement, ModelSpec, QuerySet } from "orm";
import { behaviorField, behaviorRecords, hardDeleteStatement, rowQueryOf } from "orm/ext";
import type { QuerySetCtor } from "orm/ext";

type AnyQuerySet = QuerySet<ModelSpec>;
type Deleted = { readonly column: ReturnType<typeof behaviorField>; readonly name: string };
type RowsOf<T> = T extends QuerySet<infer M, any, any, any, any, any> ? M["row"][] : never;
type Returning = { readonly returning?: boolean };

// One lookup for each model class, not one for each call.
const fields = new WeakMap<object, Deleted>();

function deletedAt(qs: AnyQuerySet): Deleted {
  const meta = qs.meta;
  let column = fields.get(meta);
  if (column === undefined) {
    const record = behaviorRecords(qs, "soft_delete")[0];
    if (record === undefined) throw new TypeError(`${meta.name} has no @soft_delete.deleted_at field`);
    column = { column: behaviorField(meta, record), name: meta.fields.get(String(record["field"]))!.name };
    fields.set(meta, column);
  }
  return column;
}

export function softDeleteQuerySet<B extends QuerySetCtor>(Base: B) {
  return class SoftDelete extends Base {
    /**
     * `DELETE` every matching row also for a model with `@soft_delete.deleted_at`. In
     * database mode, the trigger soft-deletes a live row, so this deletes only rows that are
     * already soft-deleted.
     */
    hardDelete<T extends AnyQuerySet>(this: T): InspectedStatement<number>;
    hardDelete<T extends AnyQuerySet>(this: T, options: { readonly returning: true }): InspectedStatement<RowsOf<T>>;
    hardDelete<T extends AnyQuerySet>(this: T, options: Returning = {}): InspectedStatement<any> {
      deletedAt(this);
      return hardDeleteStatement(this.withoutDefaults("deleted" as never), options);
    }

    /** Clear the soft-delete field of the matching soft-deleted rows. It bypasses the `"deleted"` default only. */
    undelete<T extends AnyQuerySet>(this: T): InspectedStatement<number>;
    undelete<T extends AnyQuerySet>(this: T, options: { readonly returning: true }): InspectedStatement<RowsOf<T>>;
    undelete<T extends AnyQuerySet>(this: T, options: Returning = {}): InspectedStatement<any> {
      const column = deletedAt(this);
      return this.withoutDefaults("deleted" as never).filter(column.column.isNotNull() as never).update({ [column.name]: null } as never, options) as never;
    }

    /** The rows with the soft-deleted ones. The other defaults, such as a tenant filter, stay. */
    allWithDeleted<T extends AnyQuerySet>(this: T): T {
      deletedAt(this);
      return this.withoutDefaults("deleted" as never) as T;
    }

    /** Only the soft-deleted rows; the other defaults stay. */
    deletedOnly<T extends AnyQuerySet>(this: T): T {
      return this.withoutDefaults("deleted" as never).filter(deletedAt(this).column.isNotNull() as never) as unknown as T;
    }
  };
}

export class SoftDeleteInstance {
  /** `DELETE ... WHERE pk = ...` also for a soft-delete model; in database mode, the trigger soft-deletes a live row. */
  async hardDelete(this: { readonly pk: unknown }): Promise<void> {
    await hardDeleteStatement(rowQueryOf(this));
  }

  /** Clear the soft-delete field of this row and refresh it. */
  async undelete(this: { readonly pk: unknown }): Promise<void> {
    const qs = rowQueryOf(this);
    await (this as any).update({ [deletedAt(qs).name]: null });
  }
}
