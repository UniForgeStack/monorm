import type { ModelSpec, QuerySet } from "@ufstack/monorm";
import { and, or, VersionConflict } from "@ufstack/monorm";
import { behaviorField, behaviorRecords, boundAtomic, boundFresh, boundGroups, boundGuarded, boundMeta, loadedValue } from "@ufstack/monorm/ext";
import type { BoundQuerySetCtor, QuerySetCtor } from "@ufstack/monorm/ext";

type Row = Record<string, unknown>;
type Version = ReturnType<typeof behaviorField>;
type Op = (qs: QuerySet<ModelSpec>) => PromiseLike<Row[]>;
type Meta = ReturnType<typeof boundMeta>;

const SOURCE = "monorm.locking";

// One lookup for each model class, not one for each call.
const versions = new WeakMap<object, Version>();

/** The name and the column of the version field. Only a model with a locking record has the mixin. */
function versionOf(meta: Meta): Version {
  let version = versions.get(meta);
  if (version === undefined) {
    const record = behaviorRecords({ meta } as never, "instance_guards").find((r) => r["source"] === SOURCE);
    if (record === undefined) throw new TypeError(`${meta.name} has no @@locking.version attribute`);
    version = behaviorField(meta, record);
    versions.set(meta, version);
  }
  return version;
}

/** The first object of each key. Two objects of one row with two versions can never both match. */
function held(bound: object, meta: Meta, version: Version): Row[] {
  return boundGroups(bound).map((group) => {
    const first = group[0] as Row;
    const value = loadedValue(first, version);
    for (const o of group) {
      if (loadedValue(o, version) !== value) throw new VersionConflict(`${meta.name} ${String(first["pk"])} is held with two versions`);
    }
    return first;
  });
}

const pairs = (meta: Meta, version: Version, objs: readonly Row[]) =>
  or(...(objs.map((o) => and(meta.keyMatch(meta.key, o["pk"]), version.column.eq(loadedValue(o, version)))) as [never])) as never;

export function lockingQuerySet<B extends QuerySetCtor>(Base: B) {
  return class Locking extends Base {};
}

/**
 * A bound write matches the loaded `(key, version)` pairs and throws `VersionConflict` when
 * the row of an object has another version. The conflict rolls the write back, so no row
 * changes: a write of several objects runs in a transaction. An instance write already
 * matches the loaded version, so it passes through.
 */
export function lockingBoundQuerySet<B extends BoundQuerySetCtor>(Base: B) {
  return class LockingBound extends Base {
    override async _write(op: Op, extra: readonly string[] = []): Promise<any> {
      if (boundGuarded(this)) return super._write(op as never, extra);
      const meta = boundMeta(this);
      const version = versionOf(meta);
      const objs = held(this, meta, version);
      const pair = pairs(meta, version, objs);
      const matching: Op = (qs) => op(qs.filter(pair));
      if (objs.length === 1) {
        const changed = await super._write(matching as never, extra);
        if (!changed.length) await this._throwIfStale(meta, version, objs);
        return changed;
      }
      return boundAtomic(this, async () => {
        const changed = (await super._write(matching as never, extra)) as unknown[];
        await this._throwIfStale(meta, version, objs.filter((o) => !changed.includes(o)));
        return changed;
      });
    }

    /** The write left these objects out: throw when their rows have another version. An object that a filter or a delete left out is not a conflict. */
    async _throwIfStale(meta: Meta, version: Version, unmatched: readonly Row[]): Promise<void> {
      if (!unmatched.length) return;
      for (const [o, row] of await boundFresh(this, unmatched, version.column)) {
        if (loadedValue(row, version) !== loadedValue(o, version)) throw new VersionConflict(`${meta.name} ${String((o as Row)["pk"])} was changed since it was loaded`);
      }
    }
  };
}

export class LockingInstance {}
