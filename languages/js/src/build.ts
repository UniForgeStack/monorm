/**
 * Objects from the rows the engine returns, built in one pass: instances (with
 * joined objects and prefetched relations attached) and `select()` rows.
 */

import { clearAbsent, markAbsent } from "./reference-absence.js";
import { key } from "./value-key.js";
import type { Database } from "./db.js";
import { fieldValue, related, type ModelMeta, type Registry } from "./model.js";
import type {
  NativeColumns,
  NativeInstances,
  NativePrefetch,
  NativePrefetched,
  NativeReturned,
  NativeRows,
  NativeSelect,
} from "./native.js";

type Obj = Record<PropertyKey, unknown>;

/** The relation key of a row: its one value, or one string for several columns (`null` when one is NULL). */
function rowKey(values: readonly unknown[], base: number, positions: readonly number[]): unknown {
  if (positions.length === 1) {
    const v = values[base + positions[0]!];
    return v === null ? null : key(v);
  }
  const parts: string[] = [];
  for (const p of positions) {
    const v = values[base + p];
    if (v === null) {
      return null;
    }
    const k = key(v);
    parts.push(`${typeof k}:${String(k)}`);
  }
  return parts.join("\u0000");
}

export class Builder {
  constructor(
    readonly registry: Registry,
    readonly db: Database | undefined,
  ) {}

  private meta(name: string | null): ModelMeta {
    if (name === null) {
      throw new TypeError("rows of a CTE have no model");
    }
    return this.registry.get(name);
  }

  /** One instance per row, joined objects attached. */
  instances(out: NativeInstances, rows: NativeRows): Obj[] {
    const { n, width, values } = rows;
    const meta = this.meta(out.model);
    const joins = out.joins.map((j) => ({ ...j, meta: this.meta(j.model) }));
    const objs: Obj[] = new Array(n);
    const joined: (Obj | null)[] = new Array(joins.length);
    for (let r = 0; r < n; r++) {
      const base = r * width;
      const root = meta.instance(values, base, this.db, out.shape);
      for (let i = 0; i < joins.length; i++) {
        const j = joins[i]!;
        // A LEFT JOIN without a match gives NULLs, the primary key included.
        const child = values[base + j.start + j.pk] === null ? null : j.meta.instance(values, base + j.start, this.db, j.shape);
        joined[i] = child;
        const parent = j.parent < 0 ? root : joined[j.parent];
        if (parent) {
          const pmeta = j.parent < 0 ? meta : joins[j.parent]!.meta;
          // A key with no row and no filter to explain it stays unset so the getter raises NotLoaded.
          const rel = pmeta.relationByIr.get(j.attr)!;
          clearAbsent(parent, rel.name);
          if (child !== null || !j.unsetWhenAbsent) related(parent)[rel.name] = child;
          else markAbsent(parent, rel.name, rel.from.map((ir) => fieldValue(parent, pmeta.fieldByIr.get(ir)!.name)));
        }
      }
      for (const a of out.annotations ?? []) root[a.name] = values[base + a.pos];
      objs[r] = root;
    }
    return objs;
  }

  /** The objects of a top-level SELECT: instances (prefetched relations attached), or
   * `select()` rows keyed by `keys`. */
  select(res: NativeSelect, keys?: readonly string[]): Obj[] {
    if ("joins" in res.output) {
      const objs = this.instances(res.output, res.rows);
      const meta = this.meta(res.output.model);
      for (const f of res.prefetched) {
        this.attach(meta, objs, res.rows, f);
      }
      return objs;
    }
    return this.columns(res.output, res.rows, keys ?? []);
  }

  /** Puts the relations of `res` on `parents`, whose key rows `res.rows` are (`prefetch()`). */
  prefetched(meta: ModelMeta, parents: Obj[], res: NativePrefetched): void {
    for (const f of res.prefetched) {
      this.attach(meta, parents, res.rows, f);
    }
  }

  private columns(out: NativeColumns, rows: NativeRows, keys: readonly string[]): Obj[] {
    const { n, width, values } = rows;
    const objs: Obj[] = new Array(n);
    for (let r = 0; r < n; r++) {
      const o: Obj = {};
      let pos = r * width;
      for (let i = 0; i < out.items.length; i++) {
        const w = out.items[i]!;
        if (w < 0) {
          o[keys[i]!] = values[pos];
          pos += 1;
        } else {
          o[keys[i]!] = this.meta(out.model).instance(values, pos, this.db);
          pos += w;
        }
      }
      objs[r] = o;
    }
    return objs;
  }

  /** Instances for rows a write returned. */
  returned(res: NativeReturned): Obj[] {
    return this.instances({ model: res.model, shape: res.shape ?? null, joins: [] }, res.rows);
  }

  /** Puts the related objects of `f` on `parents` (built from `parentRows`, in order). */
  private attach(meta: ModelMeta, parents: Obj[], parentRows: NativeRows, f: NativePrefetch): void {
    const children = this.instances(f.output, f.rows);
    const childMeta = this.meta(f.output.model);
    for (const c of f.children) {
      this.attach(childMeta, children, f.rows, c);
    }
    const groups = new Map<unknown, Obj | Obj[]>();
    const cw = f.rows.width;
    for (let i = 0; i < children.length; i++) {
      const k = rowKey(f.rows.values, i * cw, f.childKeyPos);
      if (f.many) {
        const list = groups.get(k) as Obj[] | undefined;
        if (list) {
          list.push(children[i]!);
        } else {
          groups.set(k, [children[i]!]);
        }
      } else {
        groups.set(k, children[i]!);
      }
    }
    // The relation itself (by its schema name), or a `toAttr` attribute.
    const rel = meta.relationByIr.get(f.attr);
    const back = f.back === null ? undefined : childMeta.relationByIr.get(f.back)!.name;
    const pw = parentRows.width;
    for (let i = 0; i < parents.length; i++) {
      const parent = parents[i]!;
      if (rel) clearAbsent(parent, rel.name);
      const k = rowKey(parentRows.values, i * pw, f.keyPos);
      const found = k === null ? undefined : groups.get(k);
      let value: unknown;
      if (f.many) {
        const list = (found as Obj[] | undefined) ?? [];
        if (back !== undefined) {
          for (const c of list) {
            related(c)[back] = parent;
          }
        }
        value = list;
      } else {
        if (back !== undefined && found) {
          related(found as Obj)[back] = parent;
        }
        value = found ?? null;
        // A NULL key is none; a set key with no row and no filter stays unset.
        if (found === undefined && k !== null && f.unsetWhenAbsent) {
          if (rel) markAbsent(parent, rel.name, rel.from.map((ir) => fieldValue(parent, meta.fieldByIr.get(ir)!.name)));
          continue;
        }
      }
      if (rel) {
        related(parent)[rel.name] = value;
      } else {
        parent[f.attr] = value;
      }
    }
  }
}
