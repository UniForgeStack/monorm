/** Extension language code at runtime: the contracts of installed packages and the member rules. */
import { importRuntime } from "./packages.js";
import type { ModelMeta } from "./model.js";
import type { QuerySet } from "./query.js";

/** What a generated models module passes to `define()` for one extension. */
export interface ExtensionCode {
  readonly module: Readonly<Record<string, unknown>>;
  /** The hash of the extension build that generated the files. */
  readonly hash?: string;
  /** The directory of the generated files. */
  readonly dir?: URL;
}

interface LanguageRecord {
  readonly extension: string;
  readonly version: string;
  readonly language: string;
  readonly query_set: string;
  readonly instance: string;
  readonly bound_query_set?: string;
  readonly query_set_members: readonly string[];
  readonly instance_members: readonly string[];
  readonly bound_query_set_members?: readonly string[];
  readonly overrides: readonly string[];
}

export type Mixin = (base: new (...args: any[]) => QuerySet<never>) => new (...args: any[]) => QuerySet<never>;

type Layer = { readonly owner: object | null; readonly descriptor: PropertyDescriptor };

/** Per row prototype: for each member name, the definitions from the host up to the last extension. */
const layers = new WeakMap<object, Map<PropertyKey, Layer[]>>();
const linked = new WeakSet<object>();

// `super` in an instance member reaches the definition below it on the row prototype of the receiver.
function linkSuper(prototype: object): void {
  if (linked.has(prototype)) return;
  linked.add(prototype);
  Object.setPrototypeOf(prototype, new Proxy(Object.getPrototypeOf(prototype) as object, {
    get(target, key, receiver) {
      for (let p: object | null = Object.getPrototypeOf(receiver as object) as object | null; p !== null; p = Object.getPrototypeOf(p) as object | null) {
        const stack = layers.get(p)?.get(key);
        if (stack === undefined) continue;
        const below = stack[stack.findIndex((l) => l.owner === prototype) - 1];
        if (below === undefined) break;
        return below.descriptor.get ? below.descriptor.get.call(receiver) : below.descriptor.value;
      }
      return Reflect.get(target, key, receiver);
    },
  }));
}

function installInstance(meta: ModelMeta, prototype: object): void {
  linkSuper(prototype);
  const target = meta.Row.prototype as object;
  const stacks = layers.get(target) ?? new Map<PropertyKey, Layer[]>();
  layers.set(target, stacks);
  for (const [key, descriptor] of Object.entries(Object.getOwnPropertyDescriptors(prototype))) {
    if (key === "constructor") continue;
    const existing = Object.getOwnPropertyDescriptor(target, key);
    const stack = stacks.get(key) ?? (existing === undefined ? [] : [{ owner: null, descriptor: existing }]);
    stack.push({ owner: prototype, descriptor });
    stacks.set(key, stack);
    Object.defineProperty(target, key, { ...descriptor, configurable: existing?.configurable ?? true });
  }
}

/**
 * Gives `meta` the members of the extensions `ids` (dependencies first): the query-set
 * mixins go to `meta.extensionMixins`, the instance members onto the row prototype.
 * A member name that a host member, a field, a relation or an earlier extension has is an
 * error, unless the extension declares an override.
 */
export function applyExtensions(meta: ModelMeta, ids: readonly string[], extensions: Readonly<Record<string, ExtensionCode>>, packaged: ReadonlyMap<string, LanguageRecord>, hostQuerySetMember: (name: string) => boolean): void {
  const given = new Map<string, string>();
  const mixins: Mixin[] = [];
  const bound: ((base: never) => unknown)[] = [];
  for (const id of ids) {
    const code = extensions[id];
    if (code === undefined) throw new TypeError(`${meta.name}: extension ${id} is not in the generated extensions`);
    const record = packaged.get(id);
    if (record === undefined) throw new TypeError(`generated models use extension ${id}, which the schema lock does not have; run \`orm generate\``);
    const overrides = new Set(record.overrides);
    const mixin = code.module[record.query_set];
    const instance = code.module[record.instance];
    if (typeof mixin !== "function" || typeof instance !== "function") throw new TypeError(`extension ${id} has no ${record.query_set} or ${record.instance}; run \`orm generate\``);
    const rules: [string, readonly string[], (name: string) => boolean][] = [
      ["query set", record.query_set_members, hostQuerySetMember],
      ["instance", record.instance_members, (name) => name in meta.Row.prototype],
    ];
    for (const [kind, names, host] of rules) {
      for (const name of names) {
        if (meta.fields.has(name) || meta.relations.has(name)) throw new TypeError(`${meta.name}.${name}: extension ${id} ${kind} member collides with a field or a relation`);
        if (overrides.has(name)) continue;
        if (given.has(name)) throw new TypeError(`${meta.name}.${name}: extension ${id} ${kind} member collides with a member of extension ${given.get(name)}`);
        if (host(name)) throw new TypeError(`${meta.name}.${name}: extension ${id} ${kind} member collides with a host member; declare it as an override`);
      }
    }
    for (const name of [...record.query_set_members, ...record.instance_members]) if (!given.has(name)) given.set(name, id);
    mixins.push(mixin as Mixin);
    if (record.bound_query_set) {
      const boundMixin = code.module[record.bound_query_set];
      if (typeof boundMixin !== "function") throw new TypeError(`extension ${id} has no ${record.bound_query_set}; run \`orm generate\``);
      bound.push(boundMixin as (base: never) => unknown);
    }
    installInstance(meta, (instance as { prototype: object }).prototype);
  }
  meta.extensionMixins = mixins;
  meta.boundMixins = bound;
  meta.resetQuerySet(meta.querySet);
}

interface PackageStamp {
  readonly dependencies?: Readonly<Record<string, string>>;
  readonly manifest_hash: string;
  readonly version: string;
  readonly attributes: readonly string[];
  readonly language_members: Readonly<Record<string, Omit<LanguageRecord, "extension" | "version" | "language" | "query_set_members" | "instance_members" | "bound_query_set_members">>>;
}

export function packageContracts(ir: { behavior?: unknown; [key: string]: unknown }, extensions: Record<string, ExtensionCode>, schema?: string): Map<string, LanguageRecord> {
  const stamps = (ir.behavior as { packages?: Record<string, PackageStamp> } | undefined)?.packages ?? {};
  const found = new Map<string, LanguageRecord>();
  if (!Object.keys(stamps).length) return found;
  const order: string[] = [];
  const visiting = new Set<string>();
  const visit = (id: string): void => {
    if (order.includes(id)) return;
    if (visiting.has(id)) throw new TypeError(`cyclic packaged extension dependencies at ${id}`);
    visiting.add(id);
    for (const dep of Object.keys(stamps[id]!.dependencies ?? {})) if (stamps[dep]) visit(dep);
    visiting.delete(id);
    order.push(id);
  };
  for (const id of Object.keys(stamps)) visit(id);
  for (const id of order) {
    const stamp = stamps[id]!;
    const code = extensions[id] ?? { module: importRuntime(id, schema) };
    extensions[id] = code;
    const installed = code.module.manifestHash;
    if (installed !== stamp.manifest_hash) process.emitWarning(`extension ${id}: locked manifest hash ${stamp.manifest_hash}, installed ${String(installed)}; run \`orm generate\``, "OrmStaleModelsWarning");
    const section = stamp.language_members.typescript;
    if (section === undefined) continue;
    const names = (exportName: string | undefined, mixin: boolean): string[] => {
      if (!exportName) return [];
      const value = code.module[exportName];
      if (typeof value !== "function") throw new TypeError(`extension ${id} has no ${exportName}; run \`orm generate\``);
      const Base = class {};
      const cls = mixin ? (value as (base: unknown) => { prototype: object })(Base) : value as { prototype: object };
      // Instance installation uses only own descriptors; traversing the `super` proxy would add Object members.
      if (!mixin) return Object.getOwnPropertyNames(cls.prototype).filter((name) => name !== "constructor" && !name.startsWith("_"));
      const members = new Set<string>();
      for (let proto: object | null = cls.prototype; proto !== null && proto !== Base.prototype && proto !== Object.prototype; proto = Object.getPrototypeOf(proto) as object | null) {
        for (const name of Object.getOwnPropertyNames(proto)) if (name !== "constructor" && !name.startsWith("_")) members.add(name);
      }
      return [...members];
    };
    found.set(id, { ...section, extension: id, version: stamp.version, language: "typescript",
      query_set_members: names(section.query_set, true), instance_members: names(section.instance, false),
      bound_query_set_members: names(section.bound_query_set, true),
    });
  }
  return found;
}

export function packageMembers(ir: { models: readonly { name: string }[]; behavior?: unknown }, packaged: ReadonlyMap<string, LanguageRecord>): Record<string, readonly string[]> {
  const b = ir.behavior as { packages?: Record<string, PackageStamp>; declarations?: { model: string; attribute: string }[] } & Record<string, unknown> | undefined;
  return Object.fromEntries(ir.models.map((m) => [m.name, [...packaged.keys()].filter((id) =>
    b?.declarations?.some((d) => d.model === m.name && b.packages?.[id]?.attributes.includes(d.attribute)) ||
    ["read_defaults", "delete_rewrites", "write_assignments", "instance_guards"].some((key) =>
      (b?.[key] as { model: string; source: string }[] | undefined)?.some((r) => r.model === m.name && r.source === id)))
  ]));
}
