/**
 * Nested types: value classes, column references, and the conversions between values and rows.
 *
 * A `type` block of the schema becomes an immutable value class. A model field of that type
 * is stored as one JSON column or as one column per leaf (`@storage`); the model member is the
 * same either way: a reference in queries (`Shop.address.city`) and a value on an instance.
 */

import { Decimal } from "./decimal.js";
import { DecodeError } from "./errors.js";
import { Column, Expression, Node, ParamRef, wrap, type IR, type IRContext } from "./expr.js";
import type { ColType, FieldMeta } from "./meta.js";

export type IRSub = {
  name: string;
  nullable: boolean;
  list?: boolean;
  type?: string;
  leaf?: {
    name: string;
    column: string;
    type: ColType;
    enum?: string;
    nullable?: boolean;
    default?: unknown;
    default_now?: boolean;
    default_sql?: string;
    client_default?: unknown;
  };
};
export type IRType = { name: string; storage: string; fields: IRSub[] };
/** On a model field of a nested type: the group it belongs to. */
export type IRNested = { type: string; storage: "columns" | "json"; field: string; path?: string[]; under_null?: boolean; root_required?: boolean; list?: boolean; present_with?: string[] };

const NONE = Symbol("none");
const TYPE = Symbol("nestedType");

class Sub {
  readonly name: string;
  readonly nullable: boolean;
  readonly list: boolean;
  readonly nested: string | undefined;
  readonly type: ColType | undefined;
  readonly enum: string | undefined;
  readonly default: unknown;
  /** The engine or the database fills this default (a clock, a SQL expression). */
  readonly dynamicDefault: boolean;

  constructor(ir: IRSub) {
    this.name = ir.name;
    this.nullable = ir.nullable;
    this.list = ir.list ?? false;
    this.nested = ir.type;
    this.type = ir.leaf?.type;
    this.enum = ir.leaf?.enum;
    this.default = ir.leaf !== undefined && ir.leaf.default !== undefined ? ir.leaf.default : NONE;
    this.dynamicDefault = !!(ir.leaf?.default_now || ir.leaf?.default_sql || ir.leaf?.client_default !== undefined);
  }
}

export interface NestedClass {
  new (values: Record<string, unknown>): NestedValue;
  readonly nestedType: NestedType;
}

/** The shape of one `type` block, with the conversions between its representations. */
export class NestedType {
  readonly name: string;
  readonly subs: readonly Sub[];
  readonly byName = new Map<string, Sub>();
  cls!: NestedClass;

  constructor(
    ir: IRType,
    readonly types: ReadonlyMap<string, NestedType>,
  ) {
    this.name = ir.name;
    this.subs = ir.fields.map((f) => new Sub(f));
    for (const s of this.subs) this.byName.set(s.name, s);
  }

  inner(sub: Sub): NestedType {
    return this.types.get(sub.nested!)!;
  }

  /** The type at `path` below this one. */
  at(path: readonly string[]): NestedType {
    let node: NestedType = this;
    for (const k of path) node = node.inner(node.byName.get(k)!);
    return node;
  }

  /** The subfield at `path`. */
  subAt(path: readonly string[]): Sub {
    return this.at(path.slice(0, -1)).byName.get(path[path.length - 1]!)!;
  }

  /** Whether the leaf at `rel` must be set whenever the type is set. */
  required(rel: readonly string[]): boolean {
    const sub = this.byName.get(rel[0]!)!;
    if (sub.nested === undefined) return !sub.nullable;
    return !sub.nullable && !sub.list && this.inner(sub).required(rel.slice(1));
  }

  // Values ------------------------------------------------------------------------------

  /** The subfields a value or an object names, checked against the type. */
  given(value: unknown, what: string): Map<string, unknown> {
    const out = new Map<string, unknown>();
    if (value instanceof NestedValue) {
      if (value[TYPE].name !== this.name) throw new TypeError(`${what}: expected ${this.name}, got ${value[TYPE].name}`);
      for (const s of this.subs) out.set(s.name, (value as unknown as Record<string, unknown>)[s.name]);
      return out;
    }
    if (typeof value !== "object" || value === null || Array.isArray(value) || value instanceof Date || value instanceof Node) {
      throw new TypeError(`${what}: expected ${this.name} or an object, got ${describe(value)}`);
    }
    const unknown = Object.keys(value).filter((k) => !this.byName.has(k));
    if (unknown.length) throw new TypeError(`${what}: ${this.name} has no subfield ${unknown.sort().join(", ")}`);
    for (const [k, v] of Object.entries(value)) if (v !== undefined) out.set(k, v);
    return out;
  }

  /** A caller's leaf value, checked and normalized (a number for a `bigint` leaf becomes a bigint). */
  leafValue(sub: Sub, value: unknown, what: string): unknown {
    if (value === null) {
      if (!sub.nullable) throw new TypeError(`${what}: is not nullable`);
      return null;
    }
    if (value instanceof Node || value instanceof ParamRef) return value;
    const bad = (expected: string) => new TypeError(`${what}: expected ${expected}, got ${describe(value)}`);
    if (sub.enum !== undefined) {
      if (typeof value !== "string" && typeof value !== "number") throw bad(`a value of ${sub.enum}`);
      return value;
    }
    switch (sub.type) {
      case "int":
        if (typeof value !== "number" || !Number.isInteger(value)) throw bad("an integer");
        return value;
      case "big_int":
        if (typeof value === "bigint") return value;
        if (typeof value === "number" && Number.isSafeInteger(value)) return BigInt(value);
        throw bad("a bigint");
      case "float":
        if (typeof value !== "number") throw bad("a number");
        return value;
      case "bool":
        if (typeof value !== "boolean") throw bad("a boolean");
        return value;
      case "string":
      case "text":
      case "uuid":
        if (typeof value !== "string") throw bad("a string");
        return value;
      case "date_time":
      case "date":
        if (!(value instanceof Date) || Number.isNaN(value.getTime())) throw bad("a valid Date");
        return value;
      case "decimal":
        if (value instanceof Decimal) return value;
        if (typeof value === "string" || typeof value === "number" || typeof value === "bigint") {
          try {
            return new Decimal(value.toString());
          } catch {
            throw bad("a decimal");
          }
        }
        throw bad("a Decimal");
    }
    return value;
  }

  /** A leaf value as a query parameter; inside a JSON value the engine binds a Date as text. */
  bound(sub: Sub, value: unknown, what: string, json: boolean): unknown {
    const v = this.leafValue(sub, value, what);
    if (!json || v === null || v instanceof Node || v instanceof ParamRef) return v;
    if (sub.type === "date_time" || sub.type === "date") return this.jsonLeaf(sub, v, what);
    return v;
  }

  /** A list leaf as one list parameter, each element checked like a plain leaf. */
  leafList(sub: Sub, value: unknown, what: string): unknown {
    if (value === null) return this.leafValue(sub, value, what);
    if (!Array.isArray(value)) throw new TypeError(`${what}: expected a list, got ${describe(value)}`);
    return value.map((v, i) => this.leafValue(sub, v, `${what}[${i}]`));
  }

  /** The value a value class holds for `sub`. */
  coerce(sub: Sub, value: unknown, what: string): unknown {
    if (value === null) {
      if (!sub.nullable) throw new TypeError(`${what}: is not nullable`);
      return null;
    }
    if (sub.list) {
      if (!Array.isArray(value)) throw new TypeError(`${what}: expected a list, got ${describe(value)}`);
      return Object.freeze(value.map((v, i) => this.one(sub, v, `${what}[${i}]`)));
    }
    return this.one(sub, value, what);
  }

  private one(sub: Sub, value: unknown, what: string): unknown {
    if (sub.nested === undefined) return this.leafValue(sub, value, what);
    const inner = this.inner(sub);
    if (value instanceof inner.cls) return value;
    return new inner.cls(Object.fromEntries(inner.given(value, what)));
  }

  /** Fills `target` with the subfields of `values`: an absent one is null, its default or an error. */
  init(target: object, values: Record<string, unknown>): void {
    const given = this.given(values, this.name);
    for (const s of this.subs) {
      const at = `${this.name}.${s.name}`;
      let v: unknown;
      if (given.has(s.name)) v = this.coerce(s, given.get(s.name), at);
      else if (s.nullable) v = null;
      else if (s.default !== NONE) v = this.decodeLeaf(s, s.default, this.name, s.name, []);
      else throw new TypeError(`${at}: is required`);
      Object.defineProperty(target, s.name, { value: v, enumerable: true });
    }
  }

  // JSON --------------------------------------------------------------------------------

  /** The JSON object of a value (or an object that names every required subfield). */
  toJson(value: unknown, what: string, whole = true): Record<string, unknown> {
    const given = this.given(value, what);
    const out: Record<string, unknown> = {};
    for (const s of this.subs) {
      const at = `${what}.${s.name}`;
      if (!given.has(s.name)) {
        if (whole && !s.nullable && s.default === NONE && !s.dynamicDefault) throw new TypeError(`${at}: is required`);
        if (whole && s.default !== NONE && !s.nullable) out[s.name] = this.jsonLeaf(s, this.decodeLeaf(s, s.default, this.name, s.name, []), at);
        continue;
      }
      out[s.name] = this.jsonSub(s, given.get(s.name), at);
    }
    return out;
  }

  jsonSub(s: Sub, v: unknown, at: string): unknown {
    if (v === null) {
      if (!s.nullable) throw new TypeError(`${at}: is not nullable`);
      return null;
    }
    if (s.list) {
      if (!Array.isArray(v)) throw new TypeError(`${at}: expected a list, got ${describe(v)}`);
      return v.map((x, i) => this.jsonOne(s, x, `${at}[${i}]`));
    }
    return this.jsonOne(s, v, at);
  }

  private jsonOne(s: Sub, v: unknown, at: string): unknown {
    return s.nested !== undefined ? this.inner(s).toJson(v, at) : this.jsonLeaf(s, v, at);
  }

  jsonLeaf(s: Sub, value: unknown, at: string): unknown {
    if (value === null) return null;
    const v = this.leafValue(s, value, at);
    if (v instanceof Node || v instanceof ParamRef) throw new TypeError(`${at}: an expression can't go into a whole JSON value`);
    switch (s.type) {
      case "big_int":
      case "decimal":
        return String(v);
      case "date_time":
        return `${(v as Date).toISOString().slice(0, -1)}000Z`;
      case "date":
        return (v as Date).toISOString().slice(0, 10);
    }
    return v;
  }

  /** The value of a stored JSON object; `DecodeError` says where it differs from the type. */
  fromJson(raw: unknown, model: string, field: string, path: readonly (string | number)[] = []): NestedValue {
    if (typeof raw !== "object" || raw === null || Array.isArray(raw)) {
      throw new DecodeError(model, field, path, `expected an object, got ${kind(raw)}`);
    }
    const obj = raw as Record<string, unknown>;
    const values: Record<string, unknown> = {};
    for (const s of this.subs) {
      const at = [...path, s.name];
      const v = obj[s.name];
      if (v === null || v === undefined) {
        if (!s.nullable) throw new DecodeError(model, field, at, "is null or missing but not nullable");
        values[s.name] = null;
      } else if (s.list) {
        if (!Array.isArray(v)) throw new DecodeError(model, field, at, `expected a list, got ${kind(v)}`);
        values[s.name] = Object.freeze(v.map((x, i) => this.decodeOne(s, x, model, field, [...at, i])));
      } else {
        values[s.name] = this.decodeOne(s, v, model, field, at);
      }
    }
    return make(this, values);
  }

  private decodeOne(s: Sub, v: unknown, model: string, field: string, at: (string | number)[]): unknown {
    if (s.nested !== undefined) return v === null ? null : this.inner(s).fromJson(v, model, field, at);
    return this.decodeLeaf(s, v, model, field, at);
  }

  decodeLeaf(s: Sub, v: unknown, model: string, field: string, at: readonly (string | number)[]): unknown {
    const bad = (expected: string) => new DecodeError(model, field, at, `expected ${expected}, got ${kind(v)} ${show(v)}`.slice(0, 200));
    if (s.enum !== undefined) {
      if (typeof v !== "string" && typeof v !== "number") throw bad(`a value of ${s.enum}`);
      return v;
    }
    switch (s.type) {
      case "string":
      case "text":
      case "uuid":
        if (typeof v !== "string") throw bad("a string");
        return v;
      case "int":
        if (typeof v !== "number" || !Number.isInteger(v)) throw bad("an integer");
        return v;
      case "big_int":
        if (typeof v === "bigint") return v;
        if (typeof v === "number") {
          if (!Number.isSafeInteger(v)) throw bad("an integer below 2^53 (a larger JSON number lost precision; store it as text)");
          return BigInt(v);
        }
        if (typeof v === "string" && /^-?\d+$/.test(v)) return BigInt(v);
        throw bad("an integer or integer text");
      case "float":
        if (typeof v !== "number") throw bad("a number");
        return v;
      case "bool":
        if (typeof v !== "boolean") throw bad("a boolean");
        return v;
      case "decimal":
        if (v instanceof Decimal) return v;
        if (typeof v !== "string" && typeof v !== "number" && typeof v !== "bigint") throw bad("decimal text");
        try {
          return new Decimal(v.toString());
        } catch {
          throw bad("a valid decimal");
        }
      case "date_time": {
        if (v instanceof Date) return v;
        const d = typeof v === "string" ? parseDateTime(v) : undefined;
        if (d === undefined) throw bad("a datetime string");
        return d;
      }
      case "date": {
        if (v instanceof Date) return v;
        const m = typeof v === "string" ? /^(\d{4})-(\d\d)-(\d\d)$/.exec(v) : null;
        const d = m ? new Date(Date.UTC(+m[1]!, +m[2]! - 1, +m[3]!)) : undefined;
        if (d === undefined || Number.isNaN(d.getTime()) || d.getUTCDate() !== +m![3]!) throw bad("a date string");
        return d;
      }
    }
    return v;
  }

  // Columns -----------------------------------------------------------------------------

  /** The value of leaf cells keyed by `path.join(".")`; `null` when the group is null. */
  collapse(cells: ReadonlyMap<string, unknown>, prefix: readonly string[] = [], nullable = false): NestedValue | null {
    if (nullable && this.isNull(cells, prefix)) return null;
    const values: Record<string, unknown> = {};
    for (const s of this.subs) {
      const at = [...prefix, s.name];
      values[s.name] = s.nested !== undefined ? this.inner(s).collapse(cells, at, s.nullable) : cells.get(at.join("."));
    }
    return make(this, values);
  }

  private isNull(cells: ReadonlyMap<string, unknown>, prefix: readonly string[]): boolean {
    const head = prefix.length ? `${prefix.join(".")}.` : "";
    const under = [...cells.keys()].filter((k) => k.startsWith(head));
    // The first required leaf is null exactly when the group is: a set group holds it.
    for (const k of under) {
      if (this.required(k.split(".").slice(prefix.length))) return cells.get(k) === null;
    }
    return under.every((k) => cells.get(k) === null);
  }
}

/** A value of `nt` from already checked subfields. */
function make(nt: NestedType, values: Record<string, unknown>): NestedValue {
  const o = Object.create(nt.cls.prototype) as NestedValue;
  for (const s of nt.subs) Object.defineProperty(o, s.name, { value: values[s.name], enumerable: true });
  return Object.freeze(o);
}

function kind(v: unknown): string {
  return v === null ? "null" : Array.isArray(v) ? "array" : typeof v;
}

function describe(v: unknown): string {
  return v === null ? "null" : Array.isArray(v) ? "an array" : typeof v === "object" ? (v.constructor?.name ?? "object") : typeof v;
}

function show(v: unknown): string {
  try {
    return typeof v === "bigint" ? `${v}n` : JSON.stringify(v) ?? String(v);
  } catch {
    return String(v);
  }
}

const DATE_TIME = /^(\d{4})-(\d\d)-(\d\d)[T ](\d\d):(\d\d):(\d\d)(?:\.(\d+))?(Z|[+-]\d\d(?::?\d\d)?)?$/;

/** An ISO 8601 text as the databases write it: a space or `T`, up to 6 fraction digits, `+00` or no zone (UTC). */
export function parseDateTime(text: string): Date | undefined {
  const m = DATE_TIME.exec(text);
  if (!m) return undefined;
  const [y, mo, d, h, mi, s] = [m[1], m[2], m[3], m[4], m[5], m[6]].map(Number) as [number, number, number, number, number, number];
  if (mo < 1 || mo > 12 || d < 1 || d > 31 || h > 23 || mi > 59 || s > 59) return undefined;
  const ms = Number((m[7] ?? "").padEnd(3, "0").slice(0, 3));
  let offset = 0;
  const zone = m[8];
  if (zone !== undefined && zone !== "Z") {
    const digits = zone.slice(1).replace(":", "");
    offset = (zone[0] === "-" ? -1 : 1) * (Number(digits.slice(0, 2)) * 60 + Number(digits.slice(2) || 0));
  }
  const t = Date.UTC(y, mo - 1, d, h, mi, s, ms) - offset * 60_000;
  const out = new Date(t);
  return Number.isNaN(out.getTime()) || new Date(Date.UTC(y, mo - 1, d)).getUTCDate() !== d ? undefined : out;
}

/** Base class of the value classes: immutable, compared by value. */
export abstract class NestedValue {
  /** @internal */
  get [TYPE](): NestedType {
    return (this.constructor as NestedClass).nestedType;
  }

  equals(other: unknown): boolean {
    if (!(other instanceof NestedValue) || other[TYPE] !== this[TYPE]) return false;
    return this[TYPE].subs.every((s) => same((this as never)[s.name], (other as never)[s.name]));
  }

  toJSON(): Record<string, unknown> {
    return this[TYPE].toJson(this, this[TYPE].name);
  }
}

function same(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (a instanceof Date && b instanceof Date) return a.getTime() === b.getTime();
  if (a instanceof Decimal && b instanceof Decimal) return a.eq(b);
  if (a instanceof NestedValue) return a.equals(b);
  if (Array.isArray(a) && Array.isArray(b)) return a.length === b.length && a.every((x, i) => same(x, b[i]));
  return false;
}

/** The nested types of a schema, each with its value class. */
export function buildTypes(irs: readonly IRType[]): Map<string, NestedType> {
  const types = new Map<string, NestedType>();
  for (const ir of irs) {
    const t = new NestedType(ir, types);
    types.set(t.name, t);
    const cls = {
      [t.name]: class extends NestedValue {
        static readonly nestedType = t;
        constructor(values: Record<string, unknown>) {
          super();
          t.init(this, values);
          Object.freeze(this);
        }
      },
    }[t.name]!;
    t.cls = cls as unknown as NestedClass;
  }
  return types;
}

// Model members --------------------------------------------------------------------------

/** What a model member of a nested type is: its type, storage and (for `columns`) its leaf fields. */
export interface NestedInfo {
  readonly model: string;
  /** The member's IR name: the group name of a `columns` field, the field name of a `json` one. */
  readonly field: string;
  readonly type: NestedType;
  readonly storage: "columns" | "json";
  readonly nullable: boolean;
  readonly leaves: readonly { readonly ir: string; readonly path: readonly string[] }[];
}

export class ObjectNode extends Expression<unknown, never, {}> {
  constructor(
    readonly nt: NestedType,
    readonly value: unknown,
    readonly what: string,
    readonly json: boolean,
  ) {
    super();
  }

  ir(ctx: IRContext): IR {
    return objectIr(this.nt, this.value, ctx, this.what, this.json);
  }
}

const NULL: IR = { t: "null" };

function node(ctx: IRContext, v: unknown): IR {
  return v instanceof Node ? v.ir(ctx) : v instanceof ParamRef ? wrap(v).ir(ctx) : ctx.param(v);
}

/** `Expr::Object` of a value (every subfield) or of an object (the subfields it names). */
export function objectIr(nt: NestedType, value: unknown, ctx: IRContext, what: string, json: boolean): IR {
  const given = nt.given(value, what);
  const fields: IR[] = [];
  for (const s of nt.subs) {
    if (!given.has(s.name)) continue;
    const v = given.get(s.name);
    const at = `${what}.${s.name}`;
    let value: IR;
    if (v === null) {
      if (!s.nullable) throw new TypeError(`${at}: is not nullable`);
      value = NULL;
    } else if (s.nested !== undefined && !s.list) {
      value = objectIr(nt.inner(s), v, ctx, at, json);
    } else if (s.list) {
      value = ctx.param(nt.jsonSub(s, v, at));
    } else {
      value = node(ctx, nt.bound(s, v, at, json));
    }
    fields.push({ name: s.name, value });
  }
  return { t: "object", fields };
}

/** The `Assignment`s that set a member of a nested type to `value` (a value, a patch object, null or an expression). */
export function nestedAssignments(info: NestedInfo, value: unknown, ctx: IRContext): IR[] {
  const what = `${info.model}.${info.field}`;
  const json = info.storage === "json";
  const nt = info.type;
  if (value === null) {
    if (!info.nullable) throw new TypeError(`${what}: is not nullable`);
    return [{ field: info.field, sub: [], value: NULL }];
  }
  if (value instanceof Node) return [{ field: info.field, sub: [], value: value.ir(ctx) }];
  if (value instanceof NestedValue) return [{ field: info.field, sub: [], value: objectIr(nt, value, ctx, what, json) }];
  const given = nt.given(value, what);
  if (!given.size) throw new TypeError(`${what}: a patch needs at least one subfield`);
  const out: IR[] = [];
  patch(info, nt, given, [], ctx, what, out);
  return out;
}

function patch(info: NestedInfo, nt: NestedType, given: Map<string, unknown>, prefix: string[], ctx: IRContext, what: string, out: IR[]): void {
  const json = info.storage === "json";
  for (const [key, v] of given) {
    const s = nt.byName.get(key)!;
    const path = [...prefix, key];
    const at = `${what}.${path.join(".")}`;
    if (s.list) throw new TypeError(`${at}: a list subfield can't be patched; assign the whole ${info.field}`);
    let value: IR;
    if (v === null) {
      if (!s.nullable) throw new TypeError(`${at}: is not nullable`);
      value = NULL;
    } else if (s.nested !== undefined) {
      const inner = nt.inner(s);
      if (!(v instanceof NestedValue) && !(v instanceof Node) && typeof v === "object" && !Array.isArray(v)) {
        const sub = inner.given(v, at);
        if (!sub.size) throw new TypeError(`${at}: a patch needs at least one subfield`);
        patch(info, inner, sub, path, ctx, what, out);
        continue;
      }
      value = v instanceof Node ? v.ir(ctx) : objectIr(inner, v, ctx, at, json);
    } else {
      value = node(ctx, nt.bound(s, v, at, json));
    }
    out.push({ field: info.field, sub: path, value });
  }
}

/** The leaf fields an insert sets for a `columns` member (a value, an object naming every required subfield, or null), by IR name. */
export function flattenGroup(info: NestedInfo, value: unknown): Map<string, unknown> {
  const what = `${info.model}.${info.field}`;
  const names = new Map(info.leaves.map((l) => [l.path.join("."), l.ir]));
  const out = new Map<string, unknown>();
  if (value === null) {
    if (!info.nullable) throw new TypeError(`${what}: is not nullable`);
    for (const n of names.values()) out.set(n, null);
    return out;
  }
  flatten(info.type, info.type.given(value, what), [], names, out, what);
  return out;
}

function flatten(nt: NestedType, given: Map<string, unknown>, prefix: string[], names: Map<string, string>, out: Map<string, unknown>, what: string): void {
  for (const s of nt.subs) {
    const path = [...prefix, s.name];
    const at = `${what}.${path.join(".")}`;
    let v: unknown;
    if (given.has(s.name)) v = given.get(s.name);
    else if (s.nullable) v = null;
    else if (s.default !== NONE) v = nt.decodeLeaf(s, s.default, nt.name, s.name, []);
    else if (s.dynamicDefault) continue;
    else throw new TypeError(`${at}: is required`);
    if (s.nested === undefined) {
      out.set(names.get(path.join("."))!, s.list ? nt.leafList(s, v, at) : nt.leafValue(s, v, at));
    } else if (v === null) {
      if (!s.nullable) throw new TypeError(`${at}: is not nullable`);
      const head = `${path.join(".")}.`;
      for (const [p, n] of names) if (p.startsWith(head)) out.set(n, null);
    } else {
      const inner = nt.inner(s);
      flatten(inner, inner.given(v, at), path, names, out, what);
    }
  }
}

/** The JSON object an insert stores in a `json` member. */
export function jsonInsert(info: NestedInfo, value: unknown): unknown {
  const what = `${info.model}.${info.field}`;
  if (value === null) {
    if (!info.nullable) throw new TypeError(`${what}: is not nullable`);
    return null;
  }
  return info.type.toJson(value, what);
}

/** The value of a `json` member's cell. */
export function decodeJsonCell(info: NestedInfo, raw: unknown, sub: readonly string[] = []): unknown {
  if (raw === null || raw === undefined) return null;
  const value = typeof raw === "string" ? (JSON.parse(raw) as unknown) : raw;
  return info.type.at(sub).fromJson(value, info.model, info.field, sub);
}

/** `Shop.address`, `Shop.address.geo` or `Shop.address.geo.lat`. */
export class NestedColumn extends Column<any, string, string, any> {
  /** @internal */
  constructor(
    root: Column<unknown, string>["root"],
    path: readonly string[],
    field: FieldMeta,
    text: string,
    readonly info: NestedInfo,
    readonly subPath: readonly string[],
  ) {
    super(root, path, field, text);
    if (this.whole) {
      for (const s of this.typeHere.subs) {
        if (s.name in this) continue;
        Object.defineProperty(this, s.name, { get: () => this.child(s.name), enumerable: true });
      }
    }
  }

  /** Whether this is a nested value, not a scalar. */
  get whole(): boolean {
    return !this.subPath.length || this.info.type.subAt(this.subPath).nested !== undefined;
  }

  /** The type of the value here (for a scalar: the type that holds it). */
  get typeHere(): NestedType {
    return this.whole ? this.info.type.at(this.subPath) : this.info.type.at(this.subPath.slice(0, -1));
  }

  /** A subfield by name, for a name that `Expression` already uses (`Shop.address.child("text")`). */
  child(name: string): NestedColumn {
    if (!this.whole || !this.typeHere.byName.has(name)) {
      throw new TypeError(`${this.text} has no subfield ${JSON.stringify(name)}`);
    }
    return new NestedColumn(this.root, this.path, this.field, `${this.text}.${name}`, this.info, [...this.subPath, name]);
  }

  override ir(ctx: IRContext): IR {
    return { ...super.ir(ctx), sub: [...this.subPath] };
  }

  /** The value of a selected cell. */
  decode(raw: unknown): unknown {
    if (this.whole) {
      if (this.info.storage === "json") return decodeJsonCell(this.info, raw, this.subPath);
      return raw === null || raw === undefined ? null : this.info.type.at(this.subPath).fromJson(typeof raw === "string" ? JSON.parse(raw) : raw, this.info.model, this.info.field, this.subPath);
    }
    if (raw === null || raw === undefined || this.info.storage !== "json") return raw;
    return this.typeHere.decodeLeaf(this.info.type.subAt(this.subPath), raw, this.info.model, this.info.field, this.subPath);
  }

  private conv(value: unknown): unknown {
    if (value === null || value instanceof Node || value instanceof ParamRef || this.whole || this.info.storage !== "json") return value;
    return this.typeHere.bound(this.info.type.subAt(this.subPath), value, this.text, true);
  }

  private object(value: unknown): Node {
    return new ObjectNode(this.typeHere, value, this.text, this.info.storage === "json");
  }

  private operand(value: unknown): unknown {
    return this.whole && value !== null && !(value instanceof Node) && !(value instanceof ParamRef) ? this.object(value) : this.conv(value);
  }

  override eq(value: any): any {
    return super.eq(this.operand(value));
  }

  override ne(value: any): any {
    return super.ne(this.operand(value));
  }

  override lt(value: any): any {
    return super.lt(this.conv(value) as never);
  }

  override lte(value: any): any {
    return super.lte(this.conv(value) as never);
  }

  override gt(value: any): any {
    return super.gt(this.conv(value) as never);
  }

  override gte(value: any): any {
    return super.gte(this.conv(value) as never);
  }

  override in(values: any): any {
    return super.in(Array.isArray(values) ? values.map((v) => this.conv(v)) : values);
  }

  override notIn(values: any): any {
    return super.notIn(Array.isArray(values) ? values.map((v) => this.conv(v)) : values);
  }

  override between(low: any, high: any): any {
    return super.between(this.conv(low) as never, this.conv(high) as never);
  }
}
