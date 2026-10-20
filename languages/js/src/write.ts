/** Rows and assignments for INSERT / UPDATE statements. */

import { Expression, ParamRef, wrap, type IR, type IRContext } from "./expr.js";
import { fieldValue, type ModelMeta } from "./model.js";
import { flattenGroup, jsonInsert, nestedAssignments } from "./nested.js";

/** Values of one row by TypeScript name, a to-one relation (`author: user`) giving its
 * key column. Expressions are refused unless `expressions`. */
function normalize(meta: ModelMeta, row: object, what: string, expressions: boolean): Map<string, unknown> {
  const values = new Map<string, unknown>();
  for (const [key, value] of Object.entries(row)) {
    if (value === undefined) {
      continue;
    }
    if (!expressions && (value instanceof Expression || value instanceof ParamRef)) {
      throw new TypeError(`${meta.name}.${key}: ${what} takes plain values, not expressions`);
    }
    const f = meta.inputFields.get(key);
    if (f) {
      if (f.nested === undefined || what === "update") values.set(f.ir, value);
      else if (f.nested.storage === "json") values.set(f.ir, jsonInsert(f.nested, value));
      else for (const [leaf, v] of flattenGroup(f.nested, value)) values.set(leaf, v);
      continue;
    }
    const rel = meta.relations.get(key);
    if (rel?.kind === "belongsTo") {
      const target = meta.registry.get(rel.target);
      rel.from.forEach((from, i) => {
        values.set(from, value === null ? null : fieldValue(value as object, target.fieldByIr.get(rel.to[i]!)!.name));
      });
      continue;
    }
    throw new TypeError(`${meta.name} has no field ${JSON.stringify(key)}`);
  }
  return values;
}

/**
 * Validates insert rows and aligns them on one column list: the fields any row sets
 * (IR names, schema order), each row's values in that order (`undefined`: the column's
 * default), and the fields given explicitly (the default `onConflict(..., { update: true })` columns).
 */
export function prepareRows(
  meta: ModelMeta,
  rows: readonly object[],
): { fields: string[]; rows: unknown[][]; provided: Set<string> } {
  const normalized: Map<string, unknown>[] = [];
  const provided = new Set<string>();
  for (const row of rows) {
    const values = normalize(meta, row, "insert", false);
    for (const k of values.keys()) {
      provided.add(k);
    }
    for (const f of meta.inputFieldList) {
      const parts = f.nested?.storage === "columns" ? f.nested.leaves.map((l) => meta.fieldByIr.get(l.ir)!) : [f];
      if (parts.some((p) => !values.has(p.ir) && !(p.hasInsertDefault || p.nullable))) {
        throw new TypeError(`${meta.name}.${f.name} is required`);
      }
    }
    normalized.push(values);
  }
  const fields = meta.fieldList.map((f) => f.ir).filter((n) => normalized.some((v) => v.has(n)));
  return { fields, rows: normalized.map((v) => fields.map((n) => v.get(n))), provided };
}

/** `getOrInsert`'s lookup by IR field name: plain, non-null values; a to-one relation
 * (`author: user`) gives its key field. */
export function lookupValues(meta: ModelMeta, lookup: object): Map<string, unknown> {
  const values = normalize(meta, lookup, "getOrInsert", false);
  for (const [field, value] of values) {
    if (value === null) {
      throw new TypeError(`getOrInsert: ${meta.name}.${meta.fieldByIr.get(field)!.name} is null; NULL never conflicts, so the row is not unique`);
    }
  }
  if (!values.size) {
    throw new TypeError("getOrInsert() needs the fields of a unique constraint");
  }
  return values;
}

/**
 * Validates `updateMany` rows: each has the primary key and the same fields, plain
 * values only, no primary key twice. Gives the fields (IR names, the primary key first)
 * and the rows aligned on them.
 */
export function prepareUpdateRows(meta: ModelMeta, rows: readonly object[]): { fields: string[]; rows: unknown[][] } {
  const pk = meta.key.map((f) => f.ir);
  const label = meta.key.map((f) => f.name).join(", ");
  const normalized: Map<string, unknown>[] = [];
  const seen = new Set<string>();
  let fields: Set<string> | undefined;
  rows.forEach((row, n) => {
    const values = normalize(meta, row, "updateMany", false);
    const id = pk.map((f) => values.get(f));
    if (id.some((v) => v === undefined || v === null)) {
      throw new TypeError(`updateMany row ${n} has no ${label}`);
    }
    const k = id.map((v) => (typeof v === "number" ? BigInt(v) : v)).map((v) => `${typeof v}:${String(v)}`).join("\u0000");
    if (seen.has(k)) {
      throw new TypeError(`updateMany: ${label}=${id.map(String).join(", ")} appears twice`);
    }
    seen.add(k);
    const keys = new Set(values.keys());
    if (fields === undefined) {
      if (keys.size === pk.length) {
        throw new TypeError(`updateMany rows need a field to set besides ${label}`);
      }
      fields = keys;
    } else if (keys.size !== fields.size || [...keys].some((x) => !fields!.has(x))) {
      throw new TypeError(`updateMany rows must set the same fields; row ${n} differs`);
    }
    normalized.push(values);
  });
  if (fields === undefined) {
    return { fields: [], rows: [] };
  }
  const names = [...pk, ...meta.fieldList.map((f) => f.ir).filter((f) => fields!.has(f) && !pk.includes(f))];
  return { fields: names, rows: normalized.map((v) => names.map((f) => v.get(f))) };
}

/** `SET` items as IR: plain values become parameters, expressions compile in `ctx`. */
export function assignments(meta: ModelMeta, values: object, ctx: IRContext): IR[] {
  return [...normalize(meta, values, "update", true)].flatMap(([field, value]) => {
    const nested = meta.fieldByIr.get(field)?.nested;
    return nested !== undefined ? nestedAssignments(nested, value, ctx) : [{ field, value: wrap(value).ir(ctx) }];
  });
}

/** Validate explicit local-only attachment using definition-prepared inputs. */
export function prepareAttach(meta: ModelMeta, row: object): { fields: string[]; rows: unknown[][] } {
  const local = meta.attachInputFields;
  if (local === undefined) throw new TypeError(`${meta.name} is not a composed child`);
  const values = normalize(meta, row, "attach", false);
  const allowed = new Set(local.map((f) => f.ir));
  for (const name of values.keys()) {
    if (!allowed.has(name)) throw new TypeError(`${meta.name}.${name}: attach accepts only local child fields`);
  }
  for (const field of local) {
    if (!values.has(field.ir) && !(field.hasServerValue || field.nullable)) throw new TypeError(`${meta.name}.${field.name} is required`);
  }
  const fields = local.filter((f) => values.has(f.ir)).map((f) => f.ir);
  return { fields, rows: [fields.map((f) => values.get(f))] };
}
