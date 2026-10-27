/**
 * Models: the registry, models built from a compiled schema (`define()`), and the
 * instances queries return.
 *
 * Generated `models.ts` modules call `define()` with the schema IR and give the result
 * its static types (`User`, `UserModel`, `UserSpec`, ...). `load("schema.prisma")` does
 * the same at runtime without a generated file (untyped).
 */

import { checkRequires } from "./adapters.js";
import { isAbsent } from "./reference-absence.js";
import { DoesNotExist, MultipleObjectsReturned, NotLoaded, VersionConflict } from "./errors.js";
import { Column, PATH, RelationPath, and, differs, type Condition, type NullRule, type PathState, type Source } from "./expr.js";
import { NestedColumn, buildTypes, decodeJsonCell, type IRNested, type IRType, type NestedInfo, type NestedType } from "./nested.js";
import { camel, type ColType, type FieldMeta, type ModelSpec, type RelationKind, type RelationMeta } from "./meta.js";

import { resolveApps } from "./apps.js";
import { hookPackages, resolveExtensions } from "./packages.js";
import { call, native, type NativeShape, type NativeSchema } from "./native.js";
import { key as keyOf } from "./value-key.js";
import { resolve, type Database } from "./db.js";
import * as references from "./references.js";
import { applyExtensions, packageContracts, packageMembers, type ExtensionCode, type Mixin } from "./extensions.js";
import type { BoundQuerySet, BoundQuerySetOf } from "./bound.js";
import type { InstanceLoaded, LoadItem, LockOptions, ManyRelatedSet, QuerySet, QuerySetOf, RelatedSet } from "./query.js";

/** Where an instance keeps its loaded relations. */
export const RELATED: unique symbol = Symbol("orm.related");
/** The database an instance was read from (`using(db)`), which it writes back to. */
export const DB: unique symbol = Symbol("orm.db");
/** Hidden values of a partial instance (identity, relation keys) that adapters read but ordinary property access does not show. */
export const INTERNAL: unique symbol = Symbol.for("orm.internal");
// The native artifact is fixed for the process; read its capabilities once.
const artifact = JSON.parse(native().nativeArtifact()) as { capabilities: string[]; requires?: Record<string, string[]> };
const capabilities: readonly string[] = artifact.capabilities;
checkRequires(artifact);
// Adapter selection happens once. The import is static: a top-level await would stop a synchronous require() of any package that imports `orm`.
const referenceAdapter = capabilities.includes("reference-loading") ? references : undefined;

export type IRField = {
  name: string;
  column: string;
  type: ColType;
  nullable?: boolean;
  array?: boolean;
  enum?: string;
  primary_key?: boolean;
  auto_increment?: boolean;
  hints?: Readonly<Record<string, string>>;
  unique?: boolean;
  default?: unknown;
  default_now?: boolean;
  default_sql?: string;
  /** Set on a stored field of a nested type: the group it belongs to. */
  nested?: IRNested;
  /** The ORM insert default (`@client_default`); native insert preparation fills it. */
  client_default?: unknown;
  [key: string]: unknown;
};

export type IRRelation = {
  name: string;
  kind: "one" | "many";
  target: string;
  from: string | string[];
  to: string | string[];
  foreign_key?: boolean;
  through?: { model: string; source: string; target: string };
  [key: string]: unknown;
};

export type IRModel = { name: string; table: string; fields: IRField[]; relations?: IRRelation[]; [key: string]: unknown };
export type IREnum = { name: string; storage?: string; values: { name: string; value: string | number }[]; [key: string]: unknown };
export type SchemaIR = { models: IRModel[]; enums?: IREnum[]; types?: IRType[]; [key: string]: unknown };

/** A model, as the types see it: the generated `UserModel` adds its columns and
 * relation paths. */
export interface ModelClass<M extends ModelSpec> {
  /** The root query set of the model, of its `useQuerySet` class if it has one. */
  readonly objects: QuerySetOf<M>;
  /** Schema information about the model. */
  readonly _meta: ModelMeta;
  /** `get()` found no row; `instanceof User.DoesNotExist`. */
  readonly DoesNotExist: new (message: string) => DoesNotExist;
  readonly MultipleObjectsReturned: new (message: string) => MultipleObjectsReturned;
  /** @internal */
  readonly "~spec"?: M;
}

/**
 * A unique index of one model, for `onConflict(columns, { index })`: `Task.indexes.task_one_open`.
 * The name is the index `map:` name, or the generated name of an unnamed index.
 */
export class IndexRef<M extends string = string> {
  /** @internal Brands the reference with its model, so another model's index does not type-check. */
  declare readonly "~model"?: M;
  constructor(readonly model: M, readonly name: string) {}

  toString(): string {
    return `${this.model}.indexes.${this.name}`;
  }
}

/** What every instance has, besides its columns and relations. Instances are snapshots
 * of a row: columns are read-only, writes are explicit statements. */
export interface Instance<M extends ModelSpec> {
  /** Populate fields and relations on this instance; preserve loaded values unless reload is true. */
  load(options?: { readonly reload?: boolean }): Promise<this & M["data"]>;
  load<const C extends readonly LoadItem<M>[]>(...items: C): Promise<InstanceLoaded<M, this, C>>;
  load<const C extends readonly LoadItem<M>[]>(...args: [...items: C, options: { readonly reload?: boolean }]): Promise<InstanceLoaded<M, this, C>>;
  /** The primary key's value. */
  readonly pk: M["pk"];
  /** A query set bound to this object: reads fill it in place. Same as `Model.objects.of(this)`. */
  qs(): BoundQuerySetOf<M, this>;
  /**
   * `post.qs().update(values)` for this object. Values may be expressions
   * (`{ views: Post.views.add(1) }`); the instance is filled from the returned row, so
   * it shows what the database stored. With an instance guard field, throws `VersionConflict`
   * when the row has a newer value; a missing row throws `DoesNotExist`.
   */
  update(values: M["update"]): Promise<void>;
  /**
   * `post.qs().delete()` for this object; the instance is filled from the
   * returned row (a delete rewrite shows its stored value). With an instance guard field, throws
   * `VersionConflict` when the row has a newer value.
   */
  delete(): Promise<void>;
}

export type Row = Record<PropertyKey, unknown> & { [RELATED]?: Record<string, unknown>; [DB]?: Database | undefined };

/** Schema information about one model (`User._meta`). */
export class ModelMeta implements Source {
  declare attachInputFields?: readonly FieldMeta[];
  /** The engine's read flag: reads of this model are filtered by default; set once by `define()`. */
  filtered = false;
  /** The names of the unique indexes `onConflict({ index })` takes; set once by `define()`. */
  indexNames: readonly string[] = [];
  /** Fields whose loaded value an instance update or delete must match (`instance_guards`); set once by `define()`. */
  guards: readonly FieldMeta[] = [];
  /** IR names of fields that the default `onConflict` update list leaves out (`write_assignments`); set once by `define()`. */
  upsertDrop: ReadonlySet<string> = new Set();
  /** IR names of fields that a proxy declares non-null over a nullable stored column. */
  narrowed: ReadonlySet<string> = new Set();
  readonly fields = new Map<string, FieldMeta>();
  readonly fieldByIr = new Map<string, FieldMeta>();
  /** Every stored field, the leaves of nested `columns` groups included (what the engine reads and writes). */
  readonly fieldList: FieldMeta[] = [];
  /** The fields the model shows: `fieldList` with each group's leaves replaced by one group member. */
  readonly publicList: FieldMeta[] = [];
  /** The `columns` groups, with the leaf fields of each. */
  readonly groups: FieldMeta[] = [];
  inputFields: ReadonlyMap<string, FieldMeta> = this.fields;
  inputFieldList: readonly FieldMeta[] = this.publicList;
  readonly relations = new Map<string, RelationMeta>();
  readonly relationByIr = new Map<string, RelationMeta>();
  /** The primary key fields in key order. */
  readonly key: readonly FieldMeta[];
  /** Makes instances: `new meta.Row()`, then the fields are assigned in schema order. */
  readonly Row: new () => Row;
  /** Partial-instance constructors by the shape of a result, and by its loaded public fields. */
  private partialByShape = new WeakMap<object, new () => Row>();
  private partialByFields = new Map<string, new () => Row>();
  /** The prototype of paths that reach this model (`User.posts` for `Post`). */
  readonly pathProto: object;
  readonly model: ModelClass<ModelSpec> & Record<string, unknown>;
  /** @internal The class of `objects` and of the relation sets (`useQuerySet`), or a
   * function that gives it on first use. */
  querySet: unknown;
  /** @internal The query-set mixins of extensions, dependencies first, applied over `querySet`. */
  extensionMixins: readonly Mixin[] = [];
  /** @internal The per-model extension query-set class, built on first use. */
  extensionSet: unknown;
  /** @internal The user class composed with this model's extensions. */
  composedSet: unknown;
  /** @internal The bound-query-set mixins of extensions, dependencies first. */
  boundMixins: readonly ((base: never) => unknown)[] = [];
  /** @internal The per-model bound query-set class, built on first use. */
  boundSet: unknown;
  /** @internal Relation-set classes with the `querySet` methods, by base class. */
  readonly relatedSets = new Map<unknown, unknown>();
  private rootQuerySet: QuerySet<ModelSpec> | undefined;
  private decodeRow: ((row: Row) => void) | undefined;

  constructor(
    readonly ir: IRModel,
    readonly registry: Registry,
  ) {
    const leaves = new Map<string, FieldMeta[]>();
    for (const f of ir.fields) {
      const physicalDefault = !!(f.auto_increment || f.default !== undefined || f.default_now || f.default_sql || (f.primary_key && registry.multiTable(ir.name)?.keyDefault));
      const n = f.nested;
      const base: FieldMeta = {
        name: camel(f.name),
        ir: f.name,
        type: f.type,
        nullable: f.nullable ?? false,
        array: f.array ?? false,
        enumName: f.enum,
        primaryKey: f.primary_key ?? false,
        unique: f.unique ?? false,
        hasInsertDefault: physicalDefault || f.client_default !== undefined,
        hasServerValue: physicalDefault,
      };
      if (n?.storage === "columns") {
        const group = camel(n.field);
        const fm: FieldMeta = { ...base, leaf: { group, path: n.path ?? [], underNull: n.under_null ?? false, rootRequired: n.root_required ?? false } };
        this.fieldByIr.set(fm.ir, fm);
        this.fieldList.push(fm);
        const seen = leaves.get(n.field);
        if (seen) {
          seen.push(fm);
        } else {
          leaves.set(n.field, [fm]);
          this.publicList.push(undefined as never);
        }
        continue;
      }
      const fm: FieldMeta = n ? { ...base, nested: { model: ir.name, field: f.name, type: registry.nestedType(n.type), storage: "json", nullable: base.nullable, leaves: [] } } : base;
      checkName(ir.name, fm.name, this.fields);
      this.fields.set(fm.name, fm);
      this.fieldByIr.set(fm.ir, fm);
      this.fieldList.push(fm);
      this.publicList.push(fm);
    }
    let at = 0;
    for (const f of ir.fields) {
      while (this.publicList[at] !== undefined) at++;
      const n = f.nested;
      if (n?.storage !== "columns" || this.publicList.some((p) => p?.ir === n.field)) continue;
      const members = leaves.get(n.field)!;
      const nullable = !members[0]!.leaf!.rootRequired && members.every((m) => m.leaf!.underNull);
      const info: NestedInfo = {
        model: ir.name,
        field: n.field,
        type: registry.nestedType(n.type),
        storage: "columns",
        nullable,
        leaves: members.map((m) => ({ ir: m.ir, path: m.leaf!.path })),
      };
      const group: FieldMeta = {
        name: camel(n.field),
        ir: n.field,
        type: "json",
        nullable,
        array: false,
        enumName: undefined,
        primaryKey: false,
        unique: false,
        hasInsertDefault: false,
        hasServerValue: false,
        nested: info,
      };
      checkName(ir.name, group.name, this.fields);
      this.fields.set(group.name, group);
      this.publicList[at] = group;
      this.fieldByIr.set(group.ir, group);
      this.groups.push(group);
    }
    const chain = registry.multiTable(ir.name);
    if (chain) this.attachInputFields = ir.fields.filter((f) => chain.local.has(f.name)).map((f) => this.fieldByIr.get(f.name)!);
    for (const r of ir.relations ?? []) {
      const kind: RelationKind = r.through
        ? "manyToMany"
        : r.kind === "many"
          ? "hasMany"
          : r.foreign_key
            ? "belongsTo"
            : "hasOne";
      const rm: RelationMeta = { name: camel(r.name), ir: r.name, kind, target: r.target, from: keyList(r.from), to: keyList(r.to), through: r.through };
      if (this.fields.has(rm.name)) {
        throw new TypeError(`${ir.name}.${rm.name} is both a field and a relation`);
      }
      checkName(ir.name, rm.name, this.relations);
      this.relations.set(rm.name, rm);
      this.relationByIr.set(rm.ir, rm);
    }
    const pks = this.fieldList.filter((f) => f.primaryKey);
    const order = (ir["primary_key"] as string[] | undefined)?.map((n) => this.fieldByIr.get(n)!) ?? pks;
    if (!pks.length || order.length !== pks.length || pks.some((f) => !order.includes(f))) {
      throw new TypeError(`model ${ir.name} must have exactly one primary key`);
    }
    this.key = order;
    this.pathProto = this.makePathProto();
    this.Row = this.makeRow();
    const model = Object.create(this.pathProto) as Record<PropertyKey, unknown>;
    model[PATH] = { root: this, path: [], names: [], target: ir.name } satisfies PathState;
    const name = ir.name;
    model["_meta"] = this;
    model["DoesNotExist"] = { [`${name}.DoesNotExist`]: class extends DoesNotExist {} }[`${name}.DoesNotExist`];
    model["MultipleObjectsReturned"] = {
      [`${name}.MultipleObjectsReturned`]: class extends MultipleObjectsReturned {},
    }[`${name}.MultipleObjectsReturned`];
    Object.defineProperty(model, "objects", { get: () => this.objects, enumerable: true });
    Object.defineProperty(model, Symbol.toStringTag, { value: name });
    this.model = model as never;
  }

  /** @internal The class of the instance methods, or the function that gives it, until the first instance. */
  modelMethods: unknown;

  /** @internal Names the class of the instance methods (`useModelMethods`). */
  useModelMethods(source: unknown): void {
    if (this.methodsApplied) throw new TypeError(`${this.name}: the model methods are already in use`);
    this.modelMethods = source;
    // an arrow function (no prototype) gives the class later; a class is checked now
    if ((source as { prototype?: unknown }).prototype !== undefined) this.applyMethods();
  }

  private methodsApplied = false;

  private applyMethods(): void {
    const given = this.modelMethods;
    this.modelMethods = undefined;
    const cls = (given as { prototype?: unknown }).prototype === undefined ? (given as () => unknown)() : given;
    const chain: object[] = [];
    if (typeof cls !== "function" || (cls as { prototype?: unknown }).prototype === undefined) {
      throw new TypeError(`${this.name}: a model methods class is a class, got ${String(cls)}`);
    }
    for (let p = (cls as { prototype: object }).prototype; p !== null && p !== Object.prototype; p = Object.getPrototypeOf(p) as object) chain.push(p);
    const members = new Map<PropertyKey, PropertyDescriptor>();
    for (const proto of chain.reverse()) {
      for (const key of Reflect.ownKeys(proto)) {
        if (key !== "constructor") members.set(key, Object.getOwnPropertyDescriptor(proto, key)!);
      }
    }
    const target = this.Row.prototype as object;
    for (const key of members.keys()) {
      if (key === "toString") continue; // a display override is allowed; every other member must not collide
      if (key in target || (typeof key === "string" && (RESERVED.has(key) || this.fields.has(key) || this.relations.has(key)))) {
        throw new TypeError(`${this.name}.${String(key)}: model method collision`);
      }
    }
    for (const [key, descriptor] of members) Object.defineProperty(target, key, { ...descriptor, enumerable: false });
    this.methodsApplied = true;
  }

  /** @internal The root query set, built on first use. */
  get objects(): QuerySet<ModelSpec> {
    return (this.rootQuerySet ??= makeQuerySet(this));
  }

  /** @internal A package replaces the root query set (file storage wraps it). */
  set objects(qs: QuerySet<ModelSpec>) {
    this.rootQuerySet = qs;
  }

  /** @internal Forget the root query set and relation-set classes (`useQuerySet`). */
  resetQuerySet(querySet: unknown): void {
    this.querySet = querySet;
    this.composedSet = undefined;
    this.extensionSet = undefined;
    this.boundSet = undefined;
    this.rootQuerySet = undefined;
    this.relatedSets.clear();
  }

  get name(): string {
    return this.ir.name;
  }

  get table(): string {
    return this.ir.table;
  }

  field(name: string): FieldMeta {
    const f = this.fields.get(name);
    if (!f) {
      throw new TypeError(`${this.name} has no field ${JSON.stringify(name)}`);
    }
    return f;
  }

  /** A column of this model, at the root of its own queries. */
  column(f: FieldMeta): Column<unknown, string> {
    return columnOf(this, [], f, `${this.name}.${f.name}`);
  }

  /** The primary key field of a model with a one-field key. */
  get pk(): FieldMeta {
    if (this.key.length !== 1) throw new TypeError(`${this.name} has a primary key of several fields; use _meta.key`);
    return this.key[0]!;
  }

  /** `field = value` for each of `fields`; `value` is an object keyed by field name for several fields. */
  keyMatch(fields: readonly FieldMeta[], value: unknown): Condition<string, never> {
    if (fields.length === 1) return this.column(fields[0]!).eq(value as never) as never;
    const parts = keyParts(fields.map((f) => f.name), value);
    return and(...fields.map((f) => this.column(f).eq(parts[f.name] as never))) as never;
  }

  private makePathProto(): object {
    const proto = Object.create(RelationPath.prototype) as object;
    for (const f of this.publicList) {
      Object.defineProperty(proto, f.name, {
        get(this: RelationPath<ModelSpec, string, never>) {
          const s = this[PATH];
          return columnOf(s.root, s.path, f, [s.root.name, ...s.names, f.name].join("."), s.rule, s.path.length > 0 && s.toOne === true, s.alias);
        },
        enumerable: true,
      });
    }
    const registry = this.registry;
    const owner = this;
    for (const r of this.relations.values()) {
      Object.defineProperty(proto, r.name, {
        get(this: RelationPath<ModelSpec, string, never>) {
          const s = this[PATH];
          const p = Object.create(registry.get(r.target).pathProto) as Record<PropertyKey, unknown>;
          const toOne = (r.kind === "belongsTo" || r.kind === "hasOne") && r.through === undefined;
          const state: PathState = { root: s.root, path: [...s.path, r.ir], names: [...s.names, r.name], target: r.target, toOne, ...(s.rule ? { rule: s.rule } : {}), ...(s.alias ? { alias: s.alias } : {}) };
          p[PATH] = r.kind === "belongsTo" ? { ...state, belongsTo: belongsToKey(owner, s, r) } : state;
          return p;
        },
        enumerable: true,
      });
    }
    return proto;
  }

  private makeRow(): new () => Row {
    const meta = this;
    const proto: Record<string, unknown> = {};
    for (const r of this.relations.values()) {
      const baseline = relationGetter(meta, r);
      Object.defineProperty(proto, r.name, {
        get: referenceAdapter ? referenceAdapter.getter(meta, r, baseline, INTERNAL, RELATED) : baseline,
        enumerable: false,
      });
    }
    Object.defineProperties(proto, {
      pk: {
        get(this: Row) {
          const internal = this[INTERNAL] as Record<string, unknown> | undefined;
          const part = (name: string) => (Object.hasOwn(this, name) ? this[name] : internal?.[name]);
          if (meta.key.length === 1) return part(meta.key[0]!.name);
          const out: Record<string, unknown> = {};
          for (const f of meta.key) {
            const v = part(f.name);
            if (v === null || v === undefined) return null;
            out[f.name] = v;
          }
          return out;
        },
      },
      update: { value: instanceUpdate, writable: true, configurable: true },
      delete: { value: instanceDelete, writable: true },
      load: { value: instanceLoad },
      qs: { value(this: Row) { return metaOf(this).objects.of(this as never); } },
      toJSON: {
        value(this: Row) {
          const out: Record<string, unknown> = {};
          for (const f of meta.publicList) {
            if (Object.hasOwn(this, f.name)) out[f.name] = this[f.name];
          }
          return out;
        },
        writable: true,
      },
      [Symbol.for("nodejs.util.inspect.custom")]: {
        value(this: Row) {
          const shown = meta.publicList.filter((f) => Object.hasOwn(this, f.name)).map((f) => `${f.name}: ${show(this[f.name])}`).join(", ");
          return `${meta.name} { ${shown} }`;
        },
      },
      [Symbol.toStringTag]: { value: meta.name },
    });
    referenceAdapter?.install(meta, proto, { DB, RELATED, INTERNAL, resolve, related, instanceUpdate });
    // A plain constructor: V8 gives every instance of a model the same shape.
    const Row = function (this: Row) {} as unknown as new () => Row;
    (Row as unknown as { prototype: object }).prototype = proto;
    Object.defineProperty(Row, "name", { value: meta.name });
    (proto as { constructor?: unknown }).constructor = Row;
    return Row;
  }

  /**
   * Makes partial instances of `shape`: like `Row`, but an absent field throws `NotLoaded`. Built on
   * first use per set of loaded fields; those get no accessor, so the row is built with plain stores.
   */
  private partialRow(shape: NonNullable<NativeShape>): new () => Row {
    let ctor = this.partialByShape.get(shape);
    if (ctor !== undefined) return ctor;
    const loaded = new Set<string>();
    for (const f of shape) {
      const field = this.fieldList[f.field]!;
      if (f.public && field.leaf === undefined) loaded.add(field.name);
    }
    const key = [...loaded].sort().join(",");
    ctor = this.partialByFields.get(key);
    if (ctor === undefined) {
      ctor = this.makePartialRow(loaded);
      this.partialByFields.set(key, ctor);
    }
    this.partialByShape.set(shape, ctor);
    return ctor;
  }

  /** Field accessors live only here, so whole-model rows and loaded fields keep plain assignment. */
  private makePartialRow(loaded: ReadonlySet<string>): new () => Row {
    const meta = this;
    const proto = Object.create(this.Row.prototype) as Record<string, unknown>;
    for (const f of this.publicList) {
      if (loaded.has(f.name)) continue;
      Object.defineProperty(proto, f.name, {
        get() { throw new NotLoaded(`${meta.name}.${f.name} was not loaded`); },
        set(this: Row, value: unknown) { Object.defineProperty(this, f.name, { value, writable: true, configurable: true, enumerable: true }); },
      });
    }
    const PartialRow = function (this: Row) {} as unknown as new () => Row;
    (PartialRow as unknown as { prototype: object }).prototype = proto;
    Object.defineProperty(PartialRow, "name", { value: meta.name });
    return PartialRow;
  }

  /** The instance for the row in `values` starting at `start`. */
  instance(values: unknown[], start: number, db: Database | undefined, shape?: NativeShape): Row {
    if (this.modelMethods !== undefined) this.applyMethods();
    const o = shape ? new (this.partialRow(shape))() : new this.Row();
    const fields = this.fieldList;
    const cells = this.groups.length ? new Map<string, Map<string, unknown>>() : undefined;
    const put = (f: FieldMeta, v: unknown) => {
      if (f.leaf !== undefined) {
        let g = cells!.get(f.leaf.group);
        if (g === undefined) cells!.set(f.leaf.group, (g = new Map()));
        g.set(f.leaf.path.join("."), v);
      } else {
        o[f.name] = f.nested !== undefined ? decodeJsonCell(f.nested, v) : v;
      }
    };
    if (shape) {
      const internal: Record<string, unknown> = {};
      for (const f of shape) {
        const meta = fields[f.field]!;
        if (f.public) put(meta, values[start + f.slot]);
        else internal[meta.name] = values[start + f.slot];
      }
      o[INTERNAL] = internal;
    } else {
      for (let i = 0; i < fields.length; i++) put(fields[i]!, values[start + i]);
    }
    if (cells !== undefined) {
      for (const [name, g] of cells) {
        const group = this.fields.get(name)!;
        if (g.size === group.nested!.leaves.length) o[name] = group.nested!.type.collapse(g, [], group.nullable);
      }
    }
    if (this.decodeRow !== undefined) this.decodeRow(o);
    if (db !== undefined) {
      o[DB] = db;
    }
    return o;
  }

  /**
   * Runs `decode` on each instance of this model that a query or write returns, after
   * its loaded fields are set. A partial instance has only its loaded fields as own
   * properties. For a package that reads stored values as its own type.
   */
  addRowDecoder(decode: (row: Row) => void): void {
    const previous = this.decodeRow;
    this.decodeRow = previous === undefined ? decode : (row) => { previous(row); decode(row); };
  }

  toString(): string {
    return this.name;
  }
}

/** A column of `f`: a nested member gets subfield access. */
export function columnOf(root: Column<unknown, string>["root"], path: readonly string[], f: FieldMeta, text: string, rule?: NullRule, toOne = false, alias?: Column<unknown, string>["alias"]): Column<unknown, string> {
  return f.nested !== undefined ? new NestedColumn(root, path, f, text, f.nested, [], rule, toOne, alias) : new Column(root, path, f, text, rule, toOne, alias);
}

/** Names the runtime gives meaning to on models / instances. */
export const RESERVED = new Set([
  "objects", "_meta", "DoesNotExist", "MultipleObjectsReturned", // models
  "pk", "update", "delete", "refresh", "load", "qs", "toJSON", "constructor", "toString", "then", // instances
]);

function checkName(model: string, name: string, seen: Map<string, unknown>): void {
  if (RESERVED.has(name)) {
    throw new TypeError(`${model}.${name}: the name ${name} is reserved in TypeScript models; rename it in the schema`);
  }
  if (seen.has(name)) {
    throw new TypeError(`${model} has two members named ${name} in TypeScript (camelCase)`);
  }
}

function show(v: unknown): string {
  if (typeof v === "bigint") {
    return `${v}n`;
  }
  if (v instanceof Date) {
    return v.toISOString();
  }
  return typeof v === "string" ? JSON.stringify(v) : String(v);
}

/** Loaded relations of an instance. */
export function related(o: object): Record<string, unknown> {
  const r = o as Row;
  return (r[RELATED] ??= {});
}

function relationGetter(meta: ModelMeta, r: RelationMeta): (this: Row) => unknown {
  const registry = meta.registry;
  const loadHint = `load it with load(${meta.name}.${r.name})`;
  switch (r.kind) {
    case "belongsTo": {
      const via = r.from.map((name) => meta.fieldByIr.get(name)!.name);
      return function (this) {
        const key = via.map((name) => fieldValue(this, name));
        const absent = key.some((k) => k === null);
        const loaded = this[RELATED];
        if (loaded && r.name in loaded) {
          const value = loaded[r.name] as Row | null;
          const target = registry.get(r.target);
          const to = r.to.map((name) => target.fieldByIr.get(name)!.name);
          // A default-filtered target joins as absent even when the key is set.
          if (value === null ? absent || target.filtered : to.every((name, i) => keyOf(fieldValue(value, name)) === keyOf(key[i]))) {
            return value;
          }
        } else if (absent) {
          return null;
        }
        const dangling = (loaded && r.name in loaded && loaded[r.name] === null) || isAbsent(this, r.name, key);
        throw new NotLoaded(`${meta.name}.${r.name} is not loaded${dangling ? ": the key is set and no row of the target has it" : ""}; ${loadHint}`);
      };
    }
    case "hasOne":
      return function (this) {
        const loaded = this[RELATED];
        if (loaded && r.name in loaded) {
          return loaded[r.name];
        }
        throw new NotLoaded(`${meta.name}.${r.name} is not loaded; ${loadHint}`);
      };
    case "hasMany":
      return function (this) {
        return relatedSet(r, this);
      };
    case "manyToMany":
      return function (this) {
        return manyRelatedSet(r, this);
      };
  }
}

// query.ts registers these (it imports this module, not the other way round).
let relatedSet: (r: RelationMeta, instance: object) => RelatedSet<ModelSpec, never>;
let manyRelatedSet: (r: RelationMeta, instance: object) => ManyRelatedSet<ModelSpec>;
let makeQuerySet: (meta: ModelMeta) => QuerySet<ModelSpec>;
let hostQuerySetMember: (name: string) => boolean;

/** @internal */
export function registerQueries(
  qs: typeof makeQuerySet,
  rs: typeof relatedSet,
  ms: typeof manyRelatedSet,
  has: typeof hostQuerySetMember,
): void {
  hostQuerySetMember = has;
  makeQuerySet = qs;
  relatedSet = rs;
  manyRelatedSet = ms;
}

/** The foreign key column of `r` at path `s`, and how to read the key it references. */
function belongsToKey(owner: ModelMeta, s: PathState, r: RelationMeta): NonNullable<PathState["belongsTo"]> {
  const key = r.from.map((name) => {
    const f = owner.fieldByIr.get(name)!;
    return new Column(s.root, s.path, f, [s.root.name, ...s.names, f.name].join("."), s.rule, s.path.length > 0 && s.toOne === true, s.alias);
  });
  const read = (o: object): unknown[] => {
    const target = owner.registry.get(r.target);
    if ((o?.constructor as { meta?: ModelMeta } | undefined)?.meta !== target) {
      throw new TypeError(`${[s.root.name, ...s.names, r.name].join(".")} compares with a ${r.target} instance`);
    }
    return r.to.map((name) => fieldValue(o, target.fieldByIr.get(name)!.name));
  };
  return { key, read };
}

// -- instance methods ---------------------------------------------------------------------------

function metaOf(o: Row): ModelMeta {
  return (o.constructor as unknown as { meta: ModelMeta }).meta;
}

/** @internal The loaded value of a field of an instance; throws `NotLoaded` when the field is not loaded. */
export function loadedValue(o: Row, field: { readonly name: string }): unknown {
  const value = Object.hasOwn(o, field.name) ? o[field.name] : (o[INTERNAL] as Record<string, unknown> | undefined)?.[field.name];
  if (value === undefined) throw new NotLoaded(`${metaOf(o).name}.${field.name} was not loaded`);
  return value;
}

/** The bound set of the one row of `o`: no default applies to the row, but a relation or aggregate keeps the default of its target. */
function rowBound(o: Row): BoundQuerySet<ModelSpec, Row> {
  return (o["qs"] as () => BoundQuerySet<ModelSpec, Row>)();
}

/** Run a write on the row of the bound set; the objects are filled from the returned rows. */
function writeOf(bound: BoundQuerySet<ModelSpec, Row>, op: (qs: QuerySet<ModelSpec>) => unknown, extra: readonly string[]): Promise<Row[]> {
  return (bound as unknown as { _write(op: unknown, extra: readonly string[]): Promise<Row[]> })._write(op, extra);
}

/** The fields that a write must read back: the guard fields, so the next write sees their values. */
function guardFields(meta: ModelMeta): readonly string[] {
  return meta.guards.map((f) => f.ir);
}

/** With instance guards, match only the loaded values of the row. The set is marked as guarded, so a bound-write extension does not add the same check. */
function guardBound(o: Row): BoundQuerySet<ModelSpec, Row> {
  const meta = metaOf(o);
  let bound = rowBound(o);
  for (const f of meta.guards) bound = bound.filter(meta.column(f).eq(loadedValue(o, f) as never) as never) as never;
  return meta.guards.length ? bound._guard() : bound;
}

/** No row matched a guarded write: throw when the row has other guard values. */
async function throwIfChanged(o: Row): Promise<void> {
  const meta = metaOf(o);
  for (const f of meta.guards) {
    const column = meta.column(f);
    const changed = rowBound(o).filter(differs(column, loadedValue(o, f)) as never);
    if (await changed.exists()) throw new VersionConflict(`${meta.name} ${show(o["pk"])} was changed since it was loaded`);
  }
}

async function instanceUpdate(this: Row, values: object): Promise<void> {
  if (!Object.keys(values).length) {
    return;
  }
  const meta = metaOf(this);
  for (const f of meta.guards) {
    if (Object.hasOwn(values, f.name)) {
      throw new TypeError(`${meta.name}.update() cannot set the version field ${f.name}; use ${meta.name}.objects.filter({ pk: ... }).update({ ${f.name}: ... }) to override it`);
    }
  }
  const rows = await writeOf(guardBound(this), (qs) => qs.update(values as never, { returning: true }), guardFields(meta));
  if (!rows.length) {
    await throwIfChanged(this);
    throw new (meta.model.DoesNotExist)(`${meta.name} ${show(this["pk"])} no longer exists`);
  }
}

async function instanceDelete(this: Row): Promise<void> {
  const rows = await writeOf(guardBound(this), (qs) => qs.delete({ returning: true }), guardFields(metaOf(this)));
  if (!rows.length) await throwIfChanged(this);
}

/** @internal Drops the reference cache of a relation whose key changed (`undefined` without the capability). */
export const invalidateReference = referenceAdapter?.invalidateReference;

async function instanceLoad(this: Row, ...args: unknown[]): Promise<Row> {
  const last = args.at(-1);
  const options = last !== undefined && typeof last === "object" && last !== null && Object.getPrototypeOf(last) === Object.prototype
    ? args.pop() as { readonly reload?: boolean } : {};
  const { loadInstance } = await import("./loading.js");
  const meta = metaOf(this);
  await loadInstance(this, args as LoadItem<ModelSpec>[], options.reload ?? false,
    referenceAdapter ? (owner, name, reload) => referenceAdapter.loadReference(owner, meta, meta.relations.get(name)!, reload,
      { DB, RELATED, INTERNAL, resolve, related, instanceUpdate }) : undefined, referenceAdapter?.invalidateReference);
  return this;
}

// -- registry -------------------------------------------------------------------------------------

/**
 * A set of models compiled together into one native schema. Models join the default
 * {@link registry} unless `define(..., { registry })` says otherwise, which is how one
 * process can hold two versions of a schema (e.g. in migration tests).
 */
export class Registry {
  private readonly models = new Map<string, ModelMeta>();
  private readonly enums = new Map<string, IREnum>();
  private readonly enumValues = new Map<string, Readonly<Record<string, string | number>>>();
  private dialect: string | undefined;
  private readonly extra: Record<string, unknown[]> = {};
  private behavior: Record<string, unknown> = {};
  private identities: unknown;
  private nativeSchema: NativeSchema | undefined;
  private packageDirs: Record<string, string> | undefined;
  private readonly types = new Map<string, IRType>();
  private built: Map<string, NestedType> | undefined;

  /** The nested type `name`, with its value class. */
  nestedType(name: string): NestedType {
    this.built ??= buildTypes([...this.types.values()], (n) => this.enumValues.get(n));
    const t = this.built.get(name);
    if (!t) throw new TypeError(`unknown nested type ${JSON.stringify(name)}`);
    return t;
  }

  /** The value classes of the nested types, by name. */
  valueClasses(): Record<string, unknown> {
    this.built ??= buildTypes([...this.types.values()], (n) => this.enumValues.get(n));
    return Object.fromEntries([...this.built].map(([k, t]) => [k, t.cls]));
  }

  /** @internal */
  addType(t: IRType): void {
    const known = this.types.get(t.name);
    if (known && JSON.stringify(known) !== JSON.stringify(t)) {
      throw new TypeError(`a type named ${t.name} is already registered`);
    }
    if (!known) {
      this.types.set(t.name, t);
      this.built = undefined;
      this.nativeSchema = undefined;
    }
  }

  get(name: string): ModelMeta {
    const m = this.models.get(name);
    if (!m) {
      throw new TypeError(`unknown model ${JSON.stringify(name)}; is its module imported?`);
    }
    return m;
  }

  has(name: string): boolean {
    return this.models.has(name);
  }

  [Symbol.iterator](): IterableIterator<ModelMeta> {
    return this.models.values();
  }

  /** @internal */
  add(meta: ModelMeta): void {
    if (this.models.has(meta.name)) {
      throw new TypeError(`a model named ${meta.name} is already registered`);
    }
    this.models.set(meta.name, meta);
    this.nativeSchema = undefined;
  }

  /** The shared enum members of this prepared schema, including ContentType. */
  getEnum(name: string): Readonly<Record<string, string | number>> {
    const values = this.enumValues.get(name);
    if (!values) throw new TypeError(`unknown enum ${JSON.stringify(name)}`);
    return values;
  }

  /** @internal */
  addEnum(e: IREnum): void {
    const known = this.enums.get(e.name);
    if (known && JSON.stringify(known) !== JSON.stringify(e)) {
      throw new TypeError(`an enum named ${e.name} is already registered`);
    }
    this.enums.set(e.name, e);
    if (!this.enumValues.has(e.name)) this.enumValues.set(e.name, Object.freeze(Object.fromEntries(e.values.map((v) => [v.name, v.value]))));
    this.nativeSchema = undefined;
  }

  /** @internal */
  addExtra(ir: SchemaIR): void {
    const dialect = (ir.dialect as string | undefined) ?? "postgres";
    if (this.dialect !== undefined && this.dialect !== dialect) {
      throw new TypeError("schemas in one registry must target the same database");
    }
    this.dialect = dialect;
    for (const key of ["extensions", "functions", "catalog", "partitions", "native_expression_slots"]) {
      const items = (ir[key] as unknown[] | undefined) ?? [];
      const known = (this.extra[key] ??= []);
      for (const x of items) {
        if (!known.some((k) => JSON.stringify(k) === JSON.stringify(x))) {
          known.push(x);
        }
      }
      if (!known.length) {
        delete this.extra[key];
      }
    }
    this.nativeSchema = undefined;
  }

  /** @internal */
  setIdentities(value: unknown): void { this.identities = detached(value); }

  /** @internal: normalized context is owned by this candidate. */
  setBehavior(value: unknown): void { this.behavior = detached((value ?? {}) as Record<string, unknown>); }

  private readonly chains = new Map<string, { local: Set<string>; keyDefault: boolean }>();

  /** For a child of a shared-key chain: its own field names, and whether the root key fills itself. */
  multiTable(model: string): { local: Set<string>; keyDefault: boolean } | undefined { return this.chains.get(model); }

  /** Derive every chain child from the prepared `owner_links` and `field_storage`; call after `setBehavior`. */
  deriveMultiTable(ir: SchemaIR): void {
    this.chains.clear();
    const links = (this.behavior["owner_links"] ?? []) as { child: string; parent: string }[];
    if (!links.length) return;
    const parents = new Map(links.map((l) => [l.child, l.parent]));
    const stored = new Map(((this.behavior["field_storage"] ?? []) as { model: string; field: string; owner: string }[]).map((f) => [`${f.model}\0${f.field}`, f.owner]));
    const tables = new Map(((this.behavior["storage"] as { models?: { name: string; table: string }[] } | undefined)?.models ?? []).map((m) => [m.table, m.name]));
    const ownerOf = new Map(ir.models.map((m) => [m.name, tables.get(m.table) ?? m.name]));
    const byOwner = new Map([...ir.models].reverse().map((m) => [ownerOf.get(m.name)!, m]));
    for (const model of ir.models) {
      const owner = ownerOf.get(model.name)!;
      if (!parents.has(owner)) continue;
      const local = new Set(model.fields.filter((f) => !f.primary_key && (stored.get(`${model.name}\0${f.name}`) ?? owner) === owner).map((f) => f.name));
      let root = owner;
      while (parents.has(root)) root = parents.get(root)!;
      const key = byOwner.get(root)?.fields.find((f) => f.primary_key);
      this.chains.set(model.name, { local, keyDefault: !!(key && (key.auto_increment || key.default !== undefined || key.default_now || key.default_sql)) });
    }
  }

  /** The prepared behavior record of `kind` that is not a list of per-model records, `undefined` if absent. */
  behaviorValue(kind: string): unknown { return this.behavior[kind]; }

  /** The prepared behavior records of `kind` for one model. */
  behaviorRecords(kind: string, model: string): readonly Readonly<Record<string, unknown>>[] {
    const records = this.behavior[kind];
    return Array.isArray(records) ? (records as Record<string, unknown>[]).filter((r) => r["model"] === model) : [];
  }

  /** The schema IR of every registered model. */
  ir(): SchemaIR {
    const out: SchemaIR = { models: [...this.models.values()].map((m) => m.ir) };
    if (this.enums.size) {
      out.enums = [...this.enums.values()];
    }
    if (this.types.size) {
      out.types = [...this.types.values()];
    }
    return JSON.parse(JSON.stringify({ ...out, ...(this.dialect === undefined ? {} : { dialect: this.dialect }), ...this.extra, ...(this.identities === undefined ? {} : { identities: this.identities }), ...(Object.keys(this.behavior).length ? { behavior: this.behavior } : {}) })) as SchemaIR;
  }

  /** Prepare the entire registered dependency batch. */
  prepare(): NativeSchema {
    return this.native();
  }

  /** @internal */
  candidate(): Registry {
    const next = new Registry();
    for (const [k, v] of this.models) next.models.set(k, v);
    for (const [k, v] of this.enums) next.enums.set(k, v);
    for (const [k, v] of this.enumValues) next.enumValues.set(k, v);
    for (const [k, v] of this.types) next.types.set(k, v);
    next.built = this.built;
    next.dialect = this.dialect;
    next.behavior = detached(this.behavior);
    next.packageDirs = this.packageDirs;
    next.identities = detached(this.identities);
    for (const [k, v] of Object.entries(this.extra)) next.extra[k] = [...v];
    return next;
  }

  /** @internal: called only after preparation succeeds. */
  publish(next: Registry): void {
    this.models.clear();
    for (const [k, v] of next.models) this.models.set(k, v);
    this.enums.clear();
    this.enumValues.clear();
    for (const [k, v] of next.enums) this.enums.set(k, v);
    for (const [k, v] of next.enumValues) this.enumValues.set(k, v);
    this.types.clear();
    for (const [k, v] of next.types) this.types.set(k, v);
    this.built = next.built;
    this.dialect = next.dialect;
    this.behavior = detached(next.behavior);
    this.packageDirs = next.packageDirs;
    this.identities = detached(next.identities);
    for (const k of Object.keys(this.extra)) delete this.extra[k];
    for (const [k, v] of Object.entries(next.extra)) this.extra[k] = [...v];
    this.nativeSchema = next.nativeSchema;
  }

  /** @internal: `prepared` is the prepared schema JSON these models were just built from. */
  prepareFrom(prepared: string, packages?: Record<string, string>): NativeSchema {
    this.packageDirs = packages;
    this.nativeSchema ??= call(() => new (native().Schema)(prepared, packages));
    return this.nativeSchema;
  }

  /** The compiled native schema (cached until models change). */
  native(): NativeSchema {
    this.nativeSchema ??= call(() => new (native().Schema)(JSON.stringify(this.ir()), this.packageDirs));
    return this.nativeSchema;
  }
}

/** A deep copy; `undefined` and empty plain objects skip `structuredClone`. */
function detached<T>(value: T): T {
  if (value === undefined) return value;
  if (value !== null && typeof value === "object" && Object.getPrototypeOf(value) === Object.prototype) {
    return Object.keys(value).length ? structuredClone(value) : ({} as T);
  }
  return structuredClone(value);
}

/** The default registry. */
export const registry = new Registry();

/**
 * Builds the models of a compiled schema (the JSON IR `orm compile` and {@link load}
 * produce). Generated `models.ts` modules call this and type the result.
 */
export function define(
  schema: string | SchemaIR,
  options: {
    readonly registry?: Registry;
    /** @internal Discovery origin for load(). */
    readonly extensionSchema?: string;
    readonly requiredCapabilities?: readonly string[];
    /** The generated code of each extension, by extension id. */
    readonly extensions?: Readonly<Record<string, ExtensionCode>>;
    /** The extension ids of each model, dependencies first. */
    readonly members?: Readonly<Record<string, readonly string[]>>;
  } = {},
): Record<string, ModelClass<ModelSpec> & Record<string, unknown>> {
  for (const capability of options.requiredCapabilities ?? []) {
    if (!capabilities.includes(capability)) throw new TypeError(`generated models require ${capability}; rebuild/select a compatible native artifact`);
  }
  const destination = options.registry ?? registry;
  const context = [...destination].length ? JSON.stringify(destination.ir()) : undefined;
  let prepared: string;
  try {
    prepared = call(() => native().prepareSchema(typeof schema === "string" ? schema : JSON.stringify(schema), context));
  } catch (e) {
    if (typeof schema === "string") JSON.parse(schema); // malformed JSON keeps throwing SyntaxError
    throw e;
  }
  const ir = JSON.parse(prepared) as SchemaIR;
  const extensions = { ...options.extensions };
  const packaged = packageContracts(ir, extensions, options.extensionSchema);
  const members = { ...packageMembers(ir, packaged), ...options.members };
  const reg = destination.candidate();
  reg.addExtra(ir);
  reg.setBehavior(ir.behavior);
  reg.deriveMultiTable(ir);
  reg.setIdentities(ir.identities);
  for (const e of ir.enums ?? []) {
    reg.addEnum(e);
  }
  for (const t of ir.types ?? []) {
    reg.addType(t);
  }
  const out: Record<string, ModelClass<ModelSpec> & Record<string, unknown>> = {};
  for (const m of ir.models) {
    if (destination.has(m.name)) {
      if (JSON.stringify(m) !== JSON.stringify(destination.get(m.name).ir)) throw new TypeError(`extension changed existing model ${m.name}; define dependent schemas together in a new registry`);
      continue;
    }
    const meta = new ModelMeta(m, reg);
    const records = (key: string) => ((ir.behavior as Record<string, { model: string; field: string }[] | undefined> | undefined)?.[key] ?? []).filter((d) => d.model === m.name);
    meta.guards = [...new Set(records("instance_guards").map((d) => d.field))].flatMap((f) => meta.fieldByIr.get(f) ?? []);
    meta.upsertDrop = new Set((records("write_assignments") as unknown as { field: string; on: string[]; upsert?: string }[]).filter((d) => (d.upsert ?? "drop") === "drop" && d.on.includes("upsert")).map((d) => d.field));
    const checks = (ir.behavior as { result_checks?: { model: string; field: string; non_null?: boolean }[] } | undefined)?.result_checks ?? [];
    meta.narrowed = new Set(checks.filter((c) => c.model === m.name && c.non_null).map((c) => c.field));
    const computed = new Set(((ir.behavior as { result_fields?: { model: string; field: string }[] } | undefined)?.result_fields ?? []).filter((f) => f.model === m.name).map((f) => f.field));
    if (computed.size) {
      meta.inputFieldList = meta.publicList.filter((f) => !computed.has(f.ir));
      meta.inputFields = new Map(meta.inputFieldList.map((f) => [f.name, f]));
    }
    for (const method of ((ir.behavior as { methods?: { model: string; name: string; native_function: string }[] } | undefined)?.methods ?? [])) {
      if (method.model === m.name) {
        const name = camel(method.name);
        checkName(m.name, name, meta.fields);
        if (meta.relations.has(name) || Object.prototype.hasOwnProperty.call(meta.model, name) || Object.prototype.hasOwnProperty.call(meta.Row.prototype, name)) throw new TypeError(`${m.name}.${name}: model method collision`);
        const fn = (native() as unknown as Record<string, (value: string) => unknown>)[method.native_function];
        if (typeof fn !== "function") throw new TypeError(`${m.name}.${name}: missing native method; rebuild`);
        Object.defineProperty(meta.model, name, { value: (value: string) => call(() => fn(value)), enumerable: true });
      }
    }
    const ids = members[m.name];
    if (ids?.length) applyExtensions(meta, ids, extensions, packaged, hostQuerySetMember);
    Object.defineProperty(meta.Row, "meta", { value: meta });
    reg.add(meta);
    out[m.name] = meta.model;
  }
  const flags = JSON.parse(reg.prepareFrom(prepared, hookPackages(ir.behavior as Parameters<typeof hookPackages>[0], options.extensionSchema)).readFlags()) as Record<string, { filtered: boolean; indexes?: string[] }>;
  for (const name of Object.keys(out)) {
    const meta = reg.get(name);
    meta.filtered = flags[name]?.filtered ?? false;
    meta.indexNames = flags[name]?.indexes ?? [];
    // a field named `indexes` keeps its column
    if (!("indexes" in meta.model)) {
      const refs = Object.fromEntries(meta.indexNames.map((n) => [n, new IndexRef(name, n)]));
      Object.defineProperty(meta.model, "indexes", { value: Object.freeze(refs), enumerable: true });
    }
  }
  destination.publish(reg);
  Object.defineProperty(out, "$types", { value: reg.valueClasses() });
  return out;
}

/**
 * Compiles a schema file (`schema.prisma`) and builds its models, without generated code
 * (and so without static types: generate `models.ts` for those).
 */
export function load(path: string, options: { readonly registry?: Registry } = {}): Record<string, ModelClass<ModelSpec> & Record<string, unknown>> {
  return define(call(() => native().compileSchemaFile(path, resolveApps(path), resolveExtensions(path))), { ...options, extensionSchema: path });
}

/** Like {@link load}, for schema source text. `import "app:<name>"` needs a file: use {@link load} or generate the models. */
export function loads(source: string, options: { readonly registry?: Registry } = {}): Record<string, ModelClass<ModelSpec> & Record<string, unknown>> {
  return define(call(() => native().compileSchema(source, null, resolveExtensions())), options);
}

/** The metadata of a model object, if `x` is one. */
export function modelMeta(x: unknown): ModelMeta | undefined {
  const meta = (x as { _meta?: unknown } | null)?._meta;
  return meta instanceof ModelMeta && meta.model === x ? meta : undefined;
}

/** Internal relation-key access without changing public loaded state. */
export function keyList(names: string | readonly string[]): readonly string[] {
  return typeof names === "string" ? [names] : names;
}

/** The value of a key: one field's value, or an object keyed by field name (null when a part is null). */
export function keyValue(o: object, names: readonly string[]): unknown {
  if (names.length === 1) return fieldValue(o, names[0]!);
  const out: Record<string, unknown> = {};
  for (const name of names) {
    const v = fieldValue(o, name);
    if (v === null || v === undefined) return null;
    out[name] = v;
  }
  return out;
}

/** The field values of a key value. */
export function keyParts(names: readonly string[], value: unknown): Record<string, unknown> {
  if (names.length === 1) return { [names[0]!]: value };
  if (value === null || value === undefined) return Object.fromEntries(names.map((n) => [n, null]));
  const parts = value as Record<string, unknown>;
  if (typeof value !== "object" || names.some((n) => !Object.hasOwn(parts, n))) {
    throw new TypeError(`a key of ${names.join(", ")} is an object with those fields, not ${String(value)}`);
  }
  return Object.fromEntries(names.map((n) => [n, parts[n]]));
}

export function fieldValue(o: object, name: string): unknown {
  const row = o as Row;
  if (Object.hasOwn(row, name)) return row[name];
  const internal = row[INTERNAL] as Record<string, unknown> | undefined;
  if (internal && Object.hasOwn(internal, name)) return internal[name];
  return row[name]; // prototype throws NotLoaded
}

/** A field of {@link describe}: names are the TypeScript (camelCase) ones. */
export interface FieldInfo {
  readonly name: string;
  readonly column: string;
  readonly type: ColType;
  readonly nullable: boolean;
  readonly array: boolean;
  /** The schema enum's name; its values are `registry.getEnum(name)`. */
  readonly enum: string | null;
  readonly maxLength: number | null;
  readonly primaryKey: boolean;
  readonly unique: boolean;
  /** Who fills an omitted insert value: the database, the ORM (`@client_default`), or nobody. */
  readonly default: "database" | "client" | null;
  /** Whether `insert()` takes it (computed fields are left out). */
  readonly insert: boolean;
}

/** A relation of {@link describe}. `from` and `to` are field names of this model and the target. */
export interface RelationInfo {
  readonly name: string;
  readonly kind: "belongsTo" | "hasOne" | "hasMany" | "manyToMany";
  readonly target: ModelClass<ModelSpec>;
  /** The key fields in key order, including single-field keys. */
  readonly from: readonly string[];
  readonly to: readonly string[];
  readonly through: ModelClass<ModelSpec> | null;
  /** The relation can be absent: a nullable key, or any relation but a to-one by key. */
  readonly nullable: boolean;
}

/** What {@link describe} gives. `unique` lists the unique keys, the primary key first. */
export interface ModelInfo {
  readonly name: string;
  readonly table: string;
  /** The key fields in key order, including single-field keys. */
  readonly primaryKey: readonly string[];
  readonly fields: readonly FieldInfo[];
  readonly relations: readonly RelationInfo[];
  readonly unique: readonly (readonly string[])[];
}

/**
 * Plain data about `model` for code that builds rows from it, such as a test factory:
 * its fields (type, nullability, enum, length, unique, who gives a default, whether
 * `insert()` takes it), its relations and its unique keys.
 */
export function describe(model: ModelClass<ModelSpec>): ModelInfo {
  const meta = model._meta;
  const registry = meta.registry;
  const fields: FieldInfo[] = meta.ir.fields.map((f) => {
    const fm = meta.fieldByIr.get(f.name)!;
    return {
      name: fm.name,
      column: f.column,
      type: f.type,
      nullable: fm.nullable,
      array: fm.array,
      enum: fm.enumName ?? null,
      maxLength: typeof f["max_length"] === "number" ? f["max_length"] : null,
      primaryKey: fm.primaryKey,
      unique: fm.unique || (fm.primaryKey && meta.key.length === 1),
      default: fm.hasServerValue ? "database" : f.client_default !== undefined ? "client" : null,
      insert: meta.inputFields.has(fm.name),
    };
  });
  const relations: RelationInfo[] = [...meta.relations.values()].map((r) => {
    const target = registry.get(r.target);
    return {
      name: r.name,
      kind: r.kind,
      target: target.model,
      from: r.from.map((n) => meta.fieldByIr.get(n)!.name),
      to: r.to.map((n) => target.fieldByIr.get(n)!.name),
      through: r.through ? registry.get(r.through.model).model : null,
      nullable: r.kind !== "belongsTo" || r.from.some((n) => meta.fieldByIr.get(n)!.nullable),
    };
  });
  const name = (ir: string) => meta.fieldByIr.get(ir)!.name;
  const constraints = (meta.ir["constraints"] as { kind: string; fields?: string[] }[] | undefined) ?? [];
  const key = meta.key.map((f) => f.name);
  const unique = [
    key,
    ...meta.ir.fields.filter((f) => f.unique && !(key.length === 1 && f.primary_key)).map((f) => [name(f.name)]),
    ...constraints.filter((c) => c.kind === "unique" && c.fields).map((c) => c.fields!.map(name)),
  ];
  return { name: meta.name, table: meta.table, primaryKey: key, fields, relations, unique };
}

/**
 * The column a dotted path names: `column(Bundle, "items.product.title")` is
 * `Bundle.items.product.title`. Each name before the last is a relation (to-one or
 * to-many), the last a field; the names are the TypeScript (camelCase) ones. For adapters
 * that take names from a request, such as a search or ordering filter.
 */
export function column(model: ModelClass<ModelSpec>, path: string): Column<unknown, string> {
  let meta = model._meta;
  let target: unknown = model;
  const names = path.split(".");
  for (const [i, name] of names.entries()) {
    const rel = meta.relations.get(name);
    if (i < names.length - 1 ? !rel : !meta.fields.has(name)) {
      throw new TypeError(`${meta.name} has no ${i < names.length - 1 ? "relation" : "field"} ${JSON.stringify(name)} (in ${JSON.stringify(path)})`);
    }
    target = (target as Record<string, unknown>)[name];
    if (rel) meta = meta.registry.get(rel.target);
  }
  return target as Column<unknown, string>;
}
