/**
 * The admin service: read-only resources over the models, called in memory by `testClient`.
 *
 * The admin is in progress: this module registers resources and runs list requests without a server.
 *
 * ```ts
 * import { admin } from "@ufstack/monorm/admin";
 *
 * admin.register(Post, { list: { columns: [Post.title, Post.author.name], order: [Post.createdAt.desc()] } });
 * admin.check(); // throws AdminConfigError for an unknown field or a list column through a to-many relation
 * const client = admin.testClient(db, { user: { is_superuser: true } });
 * const page = (await client.get("/admin/api/resources/Post")).json();
 * ```
 *
 * With no `register` call every concrete model is a read-only resource.
 */
import { HOST, hostDispatch, type HostFunction } from "./admin-host.js";
import * as grants from "./admin-permissions.js";
import type { RoleSeedReport, SyncReport } from "./admin-permissions.js";
import type { Database } from "./db.js";
import { ORMError } from "./errors.js";
import { asCondition, Column, Condition, Expression, IRContext, Ordering, PATH, RelationPath, resolveNullRule, Slot, SlotParams } from "./expr.js";
import type { ModelSpec } from "./meta.js";
import { registry as defaultRegistry, type ModelClass, type Registry } from "./model.js";
import { Prepared } from "./query.js";
import { call, native, wait, type NativeAdminRouter, type NativeAdminServer, type NativeAdminSite } from "./native.js";

export const DEFAULT_PREFIX = "/admin/api";

/** The config names an unknown field, a list column through a to-many relation, or a bad value. */
export class AdminConfigError extends ORMError {
  override name = "AdminConfigError";
}

/** A wrong setting or a missing or short `MONORM_ADMIN_SECRET`: `monorm admin` exits with code 2. */
export class AdminSettingsError extends ORMError {
  override name = "AdminSettingsError";
}

type AnyColumn = Column<any, any, any, any>;
type AnyModel = ModelClass<ModelSpec>;

type AnyRelation = RelationPath<any, any, any>;

export interface NamedChoice {
  readonly name: string;
  readonly label?: string;
  /** A condition such as `Note.rank.gt(1)`. It has no `param()`: the choice is a fixed condition. */
  readonly condition: unknown;
}

export interface NamedFilter {
  /** The name in the URL (`n.<name>=<choice>`). The choice `all` is reserved: it applies no condition. */
  readonly name: string;
  readonly label?: string;
  readonly choices: readonly NamedChoice[];
  /** The choice that applies when the URL names none. Without it: all rows. */
  readonly default?: string;
}

export interface ComputedColumn {
  /** The column name: list it in `columns` as a string. */
  readonly name: string;
  readonly label?: string;
  /** `int`, `bigint`, `float`, `bool`, `string`, `text`, `datetime`, `date`, `uuid`, `json` or `decimal`. */
  readonly type: string;
  readonly nullable?: boolean;
  /** An expression that the list statement computes, such as `func.lower(Note.title)`. It has no `param()`. */
  readonly expr?: unknown;
  /** Instead of `expr`: `async (keys) => values` gets the keys of one page (one call per page), one value per key. */
  readonly function?: BatchFunction;
  /** A sort header. It needs `expr` and `nullable: false`. */
  readonly sortable?: boolean;
}

export type BatchFunction = (keys: readonly any[]) => Promise<readonly unknown[]>;

export interface AggregateColumn {
  readonly name: string;
  readonly label?: string;
  /** A to-many relation of the model, such as `Author.comments`. */
  readonly relation: AnyRelation;
  readonly fn: "count" | "sum" | "min" | "max" | "avg";
  /** A field of the related model, such as `Author.comments.rank`: every function but `count` needs it. */
  readonly field?: AnyColumn;
}

export interface ListConfig {
  /** Fields of the model; a string names a computed or aggregate column. */
  readonly columns?: readonly (AnyColumn | string)[];
  /** The filter bar: a field, a field of a to-one relation, or a to-one relation (its key is the value). */
  readonly filters?: readonly (AnyColumn | AnyRelation)[];
  readonly namedFilters?: readonly NamedFilter[];
  readonly computed?: readonly ComputedColumn[];
  readonly aggregates?: readonly AggregateColumn[];
  /** The title of a column, by path (`"author.name"`) or name. */
  readonly labels?: Readonly<Record<string, string>>;
  /** What an empty value shows in the list. */
  readonly empty?: string;
  /** What an empty value shows in one column, by path (`"author.name"`) or name. */
  readonly emptyFields?: Readonly<Record<string, string>>;
  /** The to-many relations that the detail page shows as tabs. Without it: every one that leads to a resource. */
  readonly tabs?: readonly AnyRelation[];
  readonly order?: readonly (AnyColumn | Ordering<any, any>)[];
  /** Rows per page, 1 to 200 (25 by default). */
  readonly pageSize?: number;
  /** The list columns that a sort header may use. Without it every list column that can sort. */
  readonly sortable?: readonly AnyColumn[];
  /** Fields of the model that the search box matches. The strategy comes from the indexes of each field. */
  readonly search?: readonly AnyColumn[];
  /**
   * Replaces `search`. A prepared query set with `param("q")` (its rows are the matches), or
   * `async (q, limit) => keys`. One host call per request.
   */
  readonly searchWith?: SearchWith;
}

/** The keys of the matches: a key value, the key values in key order, or an object of key fields. */
export type SearchFunction = (q: string, limit: number) => Promise<readonly RecordKey[]>;
export type SearchWith = SearchFunction | Prepared<any, any, any>;

export interface BrandConfig {
  /** The name in the header and the page title (`Admin` by default). */
  readonly name?: string;
  /** The URL of the logo image. */
  readonly logo?: string;
  /** `#rgb` or `#rrggbb`. */
  readonly colors?: { readonly primary?: string; readonly accent?: string };
  /** Links in the header. */
  readonly links?: readonly { readonly label: string; readonly url: string }[];
  /** A short label such as `staging`, shown in the header. */
  readonly environment?: string;
}

/** The two `can` rules: a condition of the row, selected as a flag of every row in the list and the detail. */
export interface CanRules {
  readonly change?: unknown;
  readonly delete?: unknown;
}

/** `async (user) => condition`: the rows that this user sees, one call per request. A list of conditions means AND. */
export type QuerysetFunction = (user: any) => Promise<unknown>;

export interface ResourceConfig {
  /**
   * The rows that staff see: a prepared query set with a `param("user")` slot
   * (`Order.objects.filter(Order.ownerId.eq(param("user"))).prepare()`), or an async function `(user) => condition`
   * (one host call per request). Its filter applies to the list, the detail, the count, the search and the tabs;
   * its order sets the order of the list. A row outside the filter is a 404 on the detail. The bypass permission never lifts it.
   */
  readonly queryset?: Prepared<any, any, any> | QuerysetFunction;
  /** `{ change, delete }`: a condition each. The list and the detail carry the result for every row; the UI disables the button that a row does not allow. */
  readonly can?: CanRules;
  /** The form message of each named `@@check`, by the name of the check. A failed check without an entry shows a generic message. */
  readonly checkMessages?: Readonly<Record<string, string>>;
  /**
   * The sections of the form: one list for both forms, or `{ create, edit }`. A field that no section names trails in an
   * untitled section. A name that is not a field of the form is an error at the start of the site.
   */
  readonly form?: readonly FormSection[] | { readonly create?: readonly FormSection[]; readonly edit?: readonly FormSection[] };
  /** Fields that the form shows but never writes, in the create and the edit form. A write to one of them is a 422. */
  readonly readonly?: readonly string[];
  /**
   * The widget of a field (`blurb: "textarea"`), of a nested path (`"Geo.lat"`) or of a type (`Text`, an enum name).
   * A field beats a path, a path beats a type; the site `widgets` come last.
   */
  readonly widgets?: Readonly<Record<string, string>>;
  /**
   * The path of a JavaScript module that replaces the whole form: `export default (React) => function Form(props) {...}`.
   * The admin reads the file when the site starts and serves it to signed-in staff.
   */
  readonly formComponent?: string;
  readonly list?: ListConfig;
  /** The name in the navigation and the page heading (the model name, spaced, by default). */
  readonly title?: string;
  /** The navigation group: one of `groups` of the admin. */
  readonly group?: string;
  /** The operations that the resource allows for every user, superusers too. */
  readonly allow?: readonly ("read" | "create" | "update" | "delete")[];
  /** The `page` strategy only: add `"delete"` to split the delete permission from `manage`. */
  readonly perms?: readonly ("read" | "manage" | "delete")[];
  /** The label of the permission names of this resource, instead of the site label. */
  readonly label?: string;
  /** Names such as `"see_credential"` that become `<label>.<name>`. */
  readonly fieldPermissions?: readonly string[];
  /** Names such as `"refund"` that become `<label>.<action>_<resource>`. */
  readonly actions?: readonly string[];
}

/** `page`: `<label>.read_<resource>` and `<label>.manage_<resource>` (the default); `model`: `add_`, `change_`, `delete_`, `view_`; `access`: only `admin.access`. */
export type PermissionStrategy = "page" | "model" | "access";

export interface FormSection {
  readonly title?: string;
  readonly fields: readonly string[];
  /** A collapsed section starts closed. It needs a `title`. */
  readonly collapsed?: boolean;
}

export interface AdminOptions {
  readonly permissions?: PermissionStrategy;
  /** The label of the permission names (`admin` by default). */
  readonly label?: string;
  /** Role name to pattern grants (`{ support: ["orders.*", "-shops.see_credential"] }`) that `seedRoles` writes to the auth tables. */
  readonly roles?: Readonly<Record<string, readonly string[]>>;
  /** The name, logo, colors, header links and environment label of the UI. */
  readonly brand?: BrandConfig;
  /** A field (`Book.title`), a nested subfield (`Geo.lat`) or a type name (`Text`, an enum, a nested type) to a widget for every resource; a resource `widgets` entry wins at one level. */
  readonly widgets?: Readonly<Record<string, string>>;
  /** The order of the navigation groups; a resource names one of them with `group`. */
  readonly groups?: readonly string[];
  /** The time limit in seconds of each request's database statements (15 by default, 0 for none). */
  readonly statementTimeout?: number;
}

/** One key field, the key values in key order, or an object of key fields. */
type KeyPart = string | number | bigint | boolean | Date;
export type RecordKey = KeyPart | readonly KeyPart[] | { readonly [field: string]: KeyPart };

export type { RoleSeedReport, SyncReport };

export interface AdminUser {
  readonly is_superuser?: boolean;
  readonly [key: string]: unknown;
}

const LIST_KEYS = new Set(["columns", "order", "pageSize", "sortable", "search", "searchWith", "filters", "namedFilters", "computed", "aggregates", "labels", "empty", "emptyFields", "tabs"]);
const BRAND_KEYS = new Set(["name", "logo", "colors", "links", "environment"]);
const RESOURCE_KEYS = new Set(["queryset", "can", "list", "title", "group", "allow", "perms", "label", "fieldPermissions", "actions", "checkMessages", "form", "readonly", "widgets", "formComponent"]);

function widgetsJson(who: string, value: unknown): Record<string, string> {
  if (typeof value !== "object" || value === null || Array.isArray(value) || !Object.values(value).every((v) => typeof v === "string")) {
    throw new AdminConfigError(`${who} maps a field, a path or a type to a widget name`);
  }
  return { ...(value as Record<string, string>) };
}

function sectionJson(name: string, s: unknown): Record<string, unknown> {
  const section = s as Record<string, unknown> | null;
  if (typeof section !== "object" || section === null || !Array.isArray(section.fields) || !section.fields.every((f) => typeof f === "string")) {
    throw new AdminConfigError(`${name}: a form section is { title, fields }`);
  }
  for (const key of Object.keys(section)) {
    if (key !== "title" && key !== "fields" && key !== "collapsed") throw new AdminConfigError(`${name}: unknown section option '${key}'`);
  }
  return { fields: [...section.fields], ...(section.title === undefined ? {} : { title: section.title }), ...(section.collapsed === undefined ? {} : { collapsed: Boolean(section.collapsed) }) };
}

function readonlyJson(name: string, names: unknown): string[] {
  if (!Array.isArray(names) || !names.every((n) => typeof n === "string")) throw new AdminConfigError(`${name}: readonly is a list of field names`);
  return [...names];
}

function formComponentJson(name: string, path: unknown): string {
  if (typeof path !== "string") throw new AdminConfigError(`${name}: formComponent is the path of a JavaScript file`);
  return path;
}

function formJson(name: string, form: unknown): unknown {
  if (Array.isArray(form)) return form.map((s) => sectionJson(name, s));
  if (typeof form === "object" && form !== null) {
    return Object.fromEntries(Object.entries(form).map(([which, sections]) => {
      if (!Array.isArray(sections)) throw new AdminConfigError(`${name}: form ${which} is a list of sections`);
      return [which, sections.map((s) => sectionJson(name, s))];
    }));
  }
  throw new AdminConfigError(`${name}: form is a list of sections, or an object with create and edit`);
}

function label(model: AnyModel, ref: AnyColumn): string {
  if (ref.root.name !== model._meta.name) {
    throw new AdminConfigError(`${ref.text} belongs to ${ref.root.name}, not to ${model._meta.name}`);
  }
  return [...ref.path, ref.field.ir].join(".");
}

/** A column name: a string (a computed or aggregate column), a column or a relation of `model`. */
function refName(model: AnyModel, ref: AnyColumn | AnyRelation | string): string {
  if (typeof ref === "string") return ref;
  if (ref instanceof Column) return label(model, ref);
  const state = (ref as AnyRelation)[PATH];
  if (state === undefined) throw new AdminConfigError(`${model._meta.name}: ${String(ref)} is not a column or a relation`);
  if (state.root.name !== model._meta.name) throw new AdminConfigError(`${String(ref)} belongs to ${state.root.name}, not to ${model._meta.name}`);
  return state.path.join(".");
}

/** The engine IR of an expression or a condition. The admin keeps no parameters: a literal goes into the IR itself. */
function irOf(model: AnyModel, node: unknown, what: string): unknown {
  const where = `${model._meta.name}: ${what}`;
  const params: unknown[] = [];
  let ir: unknown;
  try {
    const value = node instanceof Condition ? resolveNullRule(node) : node instanceof Expression ? node : asCondition(node);
    ir = (value as Expression<unknown>).ir(new IRContext(model._meta as never, params));
  } catch (e) {
    throw new AdminConfigError(`${where}: ${e instanceof Error ? e.message : String(e)}`);
  }
  return inline(ir, params, where);
}

function inline(ir: unknown, params: readonly unknown[], where: string): unknown {
  if (Array.isArray(ir)) return ir.map((item) => inline(item, params, where));
  if (ir === null || typeof ir !== "object") return ir;
  const node = ir as Record<string, unknown>;
  if (node["t"] === "param") {
    const value = params[node["i"] as number];
    if (value === null) return { t: "null" };
    if (typeof value === "boolean") return { t: "const", value };
    if (typeof value === "string") return { t: "text", value };
    if (typeof value === "number" && Number.isSafeInteger(value)) return { t: "int", value };
    // The IR goes as a JSON number, which keeps an integer exactly only up to 2^53 - 1.
    if (typeof value === "bigint" && value >= BigInt(Number.MIN_SAFE_INTEGER) && value <= BigInt(Number.MAX_SAFE_INTEGER)) return { t: "int", value: Number(value) };
    throw new AdminConfigError(`${where}: a value of this type is not a literal of the admin; use a safe integer, a string or a boolean`);
  }
  return Object.fromEntries(Object.entries(node).map(([k, v]) => [k, inline(v, params, where)]));
}

function batchName(model: AnyModel, column: string): string {
  return `column.${model._meta.name}.${column}`;
}

function computedJson(model: AnyModel, items: readonly ComputedColumn[]): unknown[] {
  return items.map((item) => {
    if ((item.expr === undefined) === (item.function === undefined)) {
      throw new AdminConfigError(`${model._meta.name}: computed '${item.name}' sets expr or function, one of them`);
    }
    const { expr, function: fn, ...rest } = item;
    return {
      ...rest,
      ...(expr !== undefined ? { expr: irOf(model, expr, `computed '${item.name}'`) } : {}),
      ...(fn !== undefined ? { function: batchName(model, item.name) } : {}),
    };
  });
}

function aggregatesJson(model: AnyModel, items: readonly AggregateColumn[]): unknown[] {
  return items.map((item) => {
    const where = `${model._meta.name}: aggregate '${item.name}'`;
    const relation = item.relation as AnyRelation | undefined;
    if (relation?.[PATH] === undefined) throw new AdminConfigError(`${where}: relation takes a relation such as ${model._meta.name}.<relation>`);
    const name = refName(model, relation);
    const out: Record<string, unknown> = { name: item.name, relation: name, fn: item.fn };
    if (item.label !== undefined) out["label"] = item.label;
    if (item.field !== undefined) {
      const field = item.field;
      if (!(field instanceof Column) || field.root.name !== model._meta.name || field.path.join(".") !== name) {
        throw new AdminConfigError(`${where}: field takes a field of the relation '${name}'`);
      }
      out["field"] = field.field.ir;
    }
    return out;
  });
}

function namedJson(model: AnyModel, items: readonly NamedFilter[]): unknown[] {
  return items.map((item) => ({
    name: item.name,
    ...(item.label !== undefined ? { label: item.label } : {}),
    ...(item.default !== undefined ? { default: item.default } : {}),
    choices: item.choices.map((c) => ({
      name: c.name,
      ...(c.label !== undefined ? { label: c.label } : {}),
      expr: irOf(model, c.condition, `named filter '${item.name}'`),
    })),
  }));
}

function orderName(model: AnyModel, item: AnyColumn | Ordering<any, any>): string {
  if (item instanceof Ordering) {
    if (!(item.expr instanceof Column)) throw new AdminConfigError(`${model._meta.name}: order takes columns`);
    if (item.nulls !== undefined) throw new AdminConfigError(`${model._meta.name}: order does not take a NULLs placement`);
    return (item.descending ? "-" : "") + label(model, item.expr);
  }
  return label(model, item);
}

const SLOTS = ["user"];

/** The IR of a queryset filter: a `param("user")` becomes slot `i`, a literal goes inline. */
function slotIr(ir: unknown, params: readonly unknown[], slots: string[], where: string): unknown {
  if (Array.isArray(ir)) return ir.map((item) => slotIr(item, params, slots, where));
  if (ir === null || typeof ir !== "object") return ir;
  const node = ir as Record<string, unknown>;
  if (node["t"] === "param") {
    const value = params[node["i"] as number];
    if (!(value instanceof Slot)) return inline(node, params, where);
    if (!SLOTS.includes(value.name)) throw new AdminConfigError(`${where}: takes the param 'user', not '${value.name}'`);
    if (!slots.includes(value.name)) slots.push(value.name);
    return { t: "param", i: slots.indexOf(value.name) };
  }
  return Object.fromEntries(Object.entries(node).map(([k, v]) => [k, slotIr(v, params, slots, where)]));
}

function querysetName(model: AnyModel): string {
  return `queryset.${model._meta.name}`;
}

/** The queryset as config JSON, and the order it sets (column names) or `undefined`. */
function querysetJson(model: AnyModel, queryset: NonNullable<ResourceConfig["queryset"]>): { json: unknown; order: string[] | undefined } {
  const name = model._meta.name;
  if (typeof queryset === "function") return { json: { function: querysetName(model) }, order: undefined };
  if (!(queryset instanceof Prepared)) {
    throw new AdminConfigError(`${name}: queryset takes a prepared query set (${name}.objects.filter(...).prepare()) or an async function`);
  }
  const qs = (queryset as any)["qs"];
  const state = qs.state;
  if (qs.meta.name !== name) throw new AdminConfigError(`${name}: queryset is a query set of ${qs.meta.name}`);
  const unused: [string, boolean][] = [
    ["load()", Boolean(state.loads?.length) || state.modelFields !== undefined || state.related.length > 0 || state.prefetch.length > 0],
    ["a slice", state.limit !== undefined || state.offset !== undefined],
    ["from() or join()", state.from !== undefined || state.joins.length > 0],
    ["lock()", state.lock !== undefined],
    ["withoutDefaults()", state.withoutDefaults !== false],
  ];
  for (const [what, used] of unused) {
    if (used) throw new AdminConfigError(`${name}: queryset takes a filter and an order; it does not take ${what}`);
  }
  if (state.filters.length === 0) throw new AdminConfigError(`${name}: queryset needs a filter`);
  const where = `${name}: queryset`;
  const params = new SlotParams();
  const slots: string[] = [];
  let irs: unknown[];
  try {
    const ctx = new IRContext(model._meta as never, params);
    irs = state.filters.map((f: Condition) => slotIr((resolveNullRule(f) as Expression<unknown>).ir(ctx), params, slots, where));
  } catch (e) {
    if (e instanceof AdminConfigError) throw e;
    throw new AdminConfigError(`${where}: ${e instanceof Error ? e.message : String(e)}`);
  }
  const order = (state.order as Ordering<any, any>[]).map((o) => orderName(model, o));
  return { json: { filter: irs.length === 1 ? irs[0] : { t: "and", items: irs }, slots }, order: order.length > 0 ? order : undefined };
}

/** The host function of a callable queryset: the filter for this user, with its values inline. */
function querysetHost(model: AnyModel, fn: QuerysetFunction): HostFunction {
  return async (_database, args) => {
    const found = await fn(args.user);
    const conditions = (Array.isArray(found) ? found : [found]).filter((c) => c !== undefined && c !== null);
    if (conditions.length === 0) throw new AdminConfigError(`${model._meta.name}: the queryset function returns a condition`);
    const irs = conditions.map((c) => irOf(model, c, "queryset"));
    return { filter: irs.length === 1 ? irs[0] : { t: "and", items: irs } };
  };
}

function canJson(model: AnyModel, rules: CanRules): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [rule, condition] of Object.entries(rules)) {
    if (rule !== "change" && rule !== "delete") throw new AdminConfigError(`${model._meta.name}: can takes 'change' and 'delete', not '${rule}'`);
    if (condition !== undefined) out[rule] = irOf(model, condition, `can '${rule}'`);
  }
  return out;
}

function listJson(model: AnyModel, config: ListConfig): Record<string, unknown> {
  const name = model._meta.name;
  for (const key of Object.keys(config)) {
    if (!LIST_KEYS.has(key)) throw new AdminConfigError(`${name}: unknown list option '${key}'`);
  }
  const out: Record<string, unknown> = {};
  if (config.columns !== undefined) out["columns"] = config.columns.map((c) => refName(model, c));
  if (config.filters !== undefined) out["filters"] = config.filters.map((c) => refName(model, c));
  if (config.tabs !== undefined) out["tabs"] = config.tabs.map((c) => refName(model, c));
  if (config.namedFilters !== undefined) out["named_filters"] = namedJson(model, config.namedFilters);
  if (config.computed !== undefined) out["computed"] = computedJson(model, config.computed);
  if (config.aggregates !== undefined) out["aggregates"] = aggregatesJson(model, config.aggregates);
  if (config.labels !== undefined) out["labels"] = { ...config.labels };
  if (config.empty !== undefined) out["empty"] = config.empty;
  if (config.emptyFields !== undefined) out["empty_fields"] = { ...config.emptyFields };
  if (config.order !== undefined) out["order"] = config.order.map((item) => orderName(model, item));
  if (config.pageSize !== undefined) out["page_size"] = config.pageSize;
  if (config.sortable !== undefined) out["sortable"] = config.sortable.map((c) => label(model, c));
  if (config.search !== undefined) out["search"] = config.search.map((c) => label(model, c));
  if (config.searchWith !== undefined) out["search_with"] = searchName(model);
  return out;
}

/** The host function of a computed column: one call with the keys of the page, one value per key. */
function batchHost(model: AnyModel, fn: BatchFunction): HostFunction {
  const single = model._meta.key.length === 1;
  return async (_database, args) => {
    const keys = (args.keys as unknown[][]).map((cells) => (single ? cells[0] : cells));
    return (await fn(keys)).map(cellJson);
  };
}

function cellJson(value: unknown): unknown {
  if (value instanceof Date) return value.toISOString();
  if (typeof value === "bigint") return value.toString();
  if (Array.isArray(value)) return value.map(cellJson);
  if (value !== null && typeof value === "object" && Object.getPrototypeOf(value) === Object.prototype) {
    return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, cellJson(v)]));
  }
  return value;
}

function searchName(model: AnyModel): string {
  return `search.${model._meta.name}`;
}

/** The host function of a `searchWith`: the keys of the matches, at most `limit`. */
function searchHost(model: AnyModel, search: SearchWith): HostFunction {
  return async (_database, args) => {
    const limit = Number(args.limit);
    if (search instanceof Prepared) {
      const rows = (await (search as Prepared<any, any, any>).all({ q: args.q })) as Record<string, KeyPart>[];
      return rows.slice(0, limit).map((row) => model._meta.key.map((f) => keyValue(row[f.name]!)));
    }
    const found = await search(args.q, limit);
    return found.slice(0, limit).map(keyJson);
  };
}

/** The answer to one in-memory request. */
export class AdminResponse {
  constructor(
    readonly status: number,
    readonly text: string,
    /** Statements the engine ran for this request. */
    readonly queries: number,
    /** `[name, value]` pairs of the response headers, with every `set-cookie` (login mode only). */
    readonly headers: readonly (readonly [string, string])[] = [],
    /** The SQL of those statements, with placeholders (the `user` path only). */
    readonly sql: readonly string[] = [],
  ) {}

  /** The first header with this name (any case), or `null`. */
  header(name: string): string | null {
    return this.headers.find(([k]) => k.toLowerCase() === name.toLowerCase())?.[1] ?? null;
  }

  /** The `set-cookie` value that sets the cookie `name`, or `null`. */
  cookie(name: string): string | null {
    return this.headers.find(([k, v]) => k.toLowerCase() === "set-cookie" && v.startsWith(`${name}=`))?.[1] ?? null;
  }

  json(): any {
    return JSON.parse(this.text);
  }
}

const AUTO = Symbol("auto");
const CSRF_COOKIE = "__Host-monorm-csrf";

export interface RequestOptions {
  /** The JSON body. */
  readonly json?: unknown;
  readonly headers?: Readonly<Record<string, string>>;
  /** The CSRF token of a write: the jar's token unless given; `null` leaves the header out. */
  readonly csrf?: string | null;
  /** The `Origin` of a write: `http://localhost` unless given; `null` leaves the header out. */
  readonly origin?: string | null;
}

export interface LoginOptions {
  /** The signing secret (`MONORM_ADMIN_SECRET`): at least 32 bytes. */
  readonly secret: string;
  /** `idle_timeout` and `session_timeout` (whole seconds above 0), `origins` and `trusted_proxies`. */
  readonly settings?: Readonly<Record<string, unknown>>;
  /** The address that the rate limit sees. */
  readonly ip?: string;
}

/**
 * Sends requests to the admin router in memory: no server, no port.
 *
 * Two modes:
 *
 * - `user` is a plain value that the client trusts, with no login. Its shape is a JSON object:
 *   `{ is_superuser: true }` reads every resource (`allow` still limits it), no user is a 401, and any other user
 *   gets what its grants allow: a user with no grant is a 403, and a `{ key }` user has its grants loaded.
 * - `secret` runs the served router with login: the guard, the signed cookie, CSRF, the Origin check, the rate
 *   limit and the security headers. The client keeps a cookie jar. `login` signs in; a write sends the CSRF token
 *   of the jar and the Origin `http://localhost` unless `csrf` or `origin` says otherwise (`null` leaves the
 *   header out). `ip` is the address that the rate limit sees.
 */
export class AdminTestClient {
  readonly #user: string | null;
  readonly #login: LoginOptions | null;
  #router: NativeAdminRouter | null = null;
  /** The address that the rate limit sees. */
  ip: string;
  /** The cookie jar: copy it to another client to share a session. */
  readonly cookies = new Map<string, string>();

  /** @internal */
  constructor(
    private readonly site: NativeAdminSite,
    private readonly database: Database,
    user: AdminUser | null,
    login: LoginOptions | null = null,
    private readonly prefix: string = DEFAULT_PREFIX,
    private readonly host: Readonly<Record<string, HostFunction>> = HOST,
  ) {
    if (login !== null && user !== null) throw new TypeError("testClient takes `user` (no login) or `secret` (login), not both");
    this.#user = user === null ? null : JSON.stringify(user);
    this.#login = login;
    this.ip = login?.ip ?? "127.0.0.1";
  }

  async request(method: string, path: string, options: RequestOptions = {}): Promise<AdminResponse> {
    const login = this.#login;
    if (login === null) {
      if (Object.keys(options).some((k) => k !== "json")) throw new TypeError("the `user` client takes only a method, a path and `json`");
      const trace = new (native().Trace)();
      const sentJson = options.json === undefined ? null : JSON.stringify(options.json);
      const { status, body } = await wait(() => this.site.request(this.database.engine, method, path, this.#user, trace, hostDispatch(this.database, this.host), sentJson));
      const events = trace.take();
      return new AdminResponse(status, body, events.length, [], events.map((e) => e.sql));
    }
    this.#router ??= call(() =>
      this.site.client(this.database.engine, hostDispatch(this.database, this.host), login.secret, JSON.stringify(login.settings ?? {}), null),
    );
    const sent = new Map<string, string>([["host", "localhost"]]);
    for (const [k, v] of Object.entries(options.headers ?? {})) sent.set(k.toLowerCase(), v);
    if (this.cookies.size > 0) sent.set("cookie", [...this.cookies].map(([k, v]) => `${k}=${v}`).join("; "));
    if (!["GET", "HEAD", "OPTIONS"].includes(method)) {
      const csrf = options.csrf === undefined ? (this.cookies.get(CSRF_COOKIE) ?? "") : options.csrf;
      if (csrf !== null) sent.set("x-csrf-token", csrf);
      const origin = options.origin === undefined ? "http://localhost" : options.origin;
      if (origin !== null) sent.set("origin", origin);
    }
    const body = options.json === undefined ? null : JSON.stringify(options.json);
    if (body !== null && !sent.has("content-type")) sent.set("content-type", "application/json");
    const router = this.#router;
    const reply = await wait(() => router.request(method, path, [...sent], body, this.ip));
    for (const [name, value] of reply.headers as [string, string][]) {
      if (name.toLowerCase() !== "set-cookie") continue;
      const [key, ...rest] = value.split(";", 1)[0]!.split("=");
      const val = rest.join("=");
      if (value.includes("Max-Age=0") || val === "") this.cookies.delete(key!);
      else this.cookies.set(key!, val);
    }
    return new AdminResponse(reply.status, reply.body, 0, reply.headers as [string, string][]);
  }

  get(path: string, options?: RequestOptions): Promise<AdminResponse> {
    return this.request("GET", path, options);
  }

  post(path: string, options?: RequestOptions): Promise<AdminResponse> {
    return this.request("POST", path, options);
  }

  patch(path: string, options?: RequestOptions): Promise<AdminResponse> {
    return this.request("PATCH", path, options);
  }

  delete(path: string, options?: RequestOptions): Promise<AdminResponse> {
    return this.request("DELETE", path, options);
  }

  /** Signs in: reads the CSRF token, then posts the credentials. The jar keeps the session cookie. */
  async login(username: string, password: string): Promise<AdminResponse> {
    await this.get(`${this.prefix}/login`);
    return this.post(`${this.prefix}/login`, { json: { username, password } });
  }

  logout(): Promise<AdminResponse> {
    return this.post(`${this.prefix}/logout`, { json: {} });
  }
}

function keyValue(value: KeyPart): string | number | boolean {
  if (value instanceof Date) return value.toISOString();
  return typeof value === "bigint" ? value.toString() : value;
}

function keyJson(key: RecordKey): unknown {
  if (Array.isArray(key)) return key.map(keyValue);
  if (typeof key === "object" && !(key instanceof Date)) return Object.fromEntries(Object.entries(key).map(([k, v]) => [k, keyValue(v)]));
  return keyValue(key as KeyPart);
}

/** The resources of one registry. */
export class Admin {
  prefix: string = DEFAULT_PREFIX;
  readonly #resources = new Map<string, { model: AnyModel; config: ResourceConfig }>();
  readonly #strategy: PermissionStrategy;
  readonly #label: string;
  readonly #roles: Record<string, string[]>;
  readonly #brand: BrandConfig | undefined;
  readonly #widgets: Record<string, string> | undefined;
  readonly #groups: string[];
  readonly #statementTimeout: number;
  #planned: { schema: unknown; site: NativeAdminSite } | null = null;

  constructor(
    private readonly registry: Registry = defaultRegistry,
    options: AdminOptions = {},
  ) {
    this.#strategy = options.permissions ?? "page";
    this.#label = options.label ?? "admin";
    for (const key of Object.keys(options.brand ?? {})) {
      if (!BRAND_KEYS.has(key)) throw new AdminConfigError(`unknown brand option '${key}'`);
    }
    this.#brand = options.brand;
    this.#widgets = options.widgets === undefined ? undefined : widgetsJson("widgets", options.widgets);
    this.#groups = [...(options.groups ?? [])];
    const timeout = options.statementTimeout ?? 15;
    if (!(timeout >= 0) || !Number.isFinite(timeout)) throw new AdminConfigError("statementTimeout is a number of seconds, 0 or more (0 sets no limit)");
    this.#statementTimeout = timeout;
    this.#roles = {};
    for (const [name, patterns] of Object.entries(options.roles ?? {})) {
      if (typeof patterns === "string" || !Array.isArray(patterns) || !patterns.every((p) => typeof p === "string")) {
        throw new AdminConfigError(`roles[${JSON.stringify(name)}] must be a list of patterns`);
      }
      this.#roles[name] = [...patterns];
    }
  }

  /** Makes `model` a resource. Once any model is registered, only registered models are resources. */
  register(model: AnyModel, config: ResourceConfig = {}): void {
    const name = model._meta.name;
    if (this.#resources.has(name)) throw new AdminConfigError(`${name} is already registered`);
    for (const key of Object.keys(config)) {
      if (!RESOURCE_KEYS.has(key)) throw new AdminConfigError(`${name}: unknown option '${key}'`);
    }
    this.#resources.set(name, { model, config });
    this.#planned = null;
  }

  #configJson(): string {
    const resources = [...this.#resources].map(([model, { model: cls, config }]) => {
      const queryset = config.queryset === undefined ? undefined : querysetJson(cls, config.queryset);
      if (queryset?.order !== undefined && config.list?.order !== undefined) {
        throw new AdminConfigError(`${model}: the order is set twice: by the queryset and by list.order`);
      }
      const list = config.list === undefined && queryset?.order === undefined ? undefined : { ...(config.list === undefined ? {} : listJson(cls, config.list)), ...(queryset?.order === undefined ? {} : { order: queryset.order }) };
      return {
        model,
        ...(queryset === undefined ? {} : { queryset: queryset.json }),
        ...(config.can === undefined ? {} : { can: canJson(cls, config.can) }),
        ...(config.checkMessages === undefined ? {} : { check_messages: { ...config.checkMessages } }),
        ...(config.form === undefined ? {} : { form: formJson(model, config.form) }),
        ...(config.readonly === undefined ? {} : { readonly: readonlyJson(model, config.readonly) }),
        ...(config.widgets === undefined ? {} : { widgets: widgetsJson(`${model}: widgets`, config.widgets) }),
        ...(config.formComponent === undefined ? {} : { form_component: formComponentJson(model, config.formComponent) }),
        ...(list === undefined ? {} : { list }),
        ...(config.title === undefined ? {} : { title: config.title }),
        ...(config.group === undefined ? {} : { group: config.group }),
        ...(config.allow === undefined ? {} : { allow: [...config.allow] }),
        ...(config.perms === undefined ? {} : { perms: [...config.perms] }),
        ...(config.label === undefined ? {} : { label: config.label }),
        ...(config.fieldPermissions === undefined ? {} : { field_permissions: [...config.fieldPermissions] }),
        ...(config.actions === undefined ? {} : { actions: [...config.actions] }),
      };
    });
    return JSON.stringify({
      prefix: this.prefix,
      permissions: this.#strategy,
      label: this.#label,
      resources,
      ...(this.#brand === undefined ? {} : { brand: this.#brand }),
      ...(this.#widgets === undefined || Object.keys(this.#widgets).length === 0 ? {} : { widgets: this.#widgets }),
      ...(this.#groups.length === 0 ? {} : { groups: this.#groups }),
      ...(this.#statementTimeout === 15 ? {} : { statement_timeout_ms: Math.round(this.#statementTimeout * 1000) }),
    });
  }

  /** The host functions: login, the `searchWith` function of each resource and the function of each computed column. */
  #host(): Record<string, HostFunction> {
    const host: Record<string, HostFunction> = { ...HOST };
    for (const [, { model, config }] of this.#resources) {
      if (typeof config.queryset === "function") host[querysetName(model)] = querysetHost(model, config.queryset);
      const search = config.list?.searchWith;
      if (search !== undefined) host[searchName(model)] = searchHost(model, search);
      for (const column of config.list?.computed ?? []) {
        if (column.function !== undefined) host[batchName(model, column.name)] = batchHost(model, column.function);
      }
    }
    return host;
  }

  #site(registry: Registry): NativeAdminSite {
    const schema = registry.native();
    try {
      return call(() => native().adminSite(schema, this.#configJson()));
    } catch (e) {
      if (e instanceof AdminConfigError) throw e;
      throw new AdminConfigError(e instanceof Error ? e.message : String(e));
    }
  }

  /**
   * Throws `AdminConfigError` when the config does not fit the schema. Needs no database.
   * Returns the warnings, for example a search field that no index serves. `monorm admin` logs the same warnings at start.
   */
  check(): string[] {
    return [...this.#site(this.registry).warnings];
  }

  /**
   * The path of the page of one record: `/admin/<Model>/<key>`. `key` is the value of a single key field, the key values in
   * key order, or an object of key fields. Throws `AdminConfigError` when `model` is no resource and `Error` when the key does not fit.
   */
  url(model: AnyModel, key: RecordKey): string {
    const schema = this.registry.native();
    if (this.#planned === null || this.#planned.schema !== schema) this.#planned = { schema, site: this.#site(this.registry) };
    try {
      return call(() => this.#planned!.site.url(model._meta.name, JSON.stringify(keyJson(key))));
    } catch (e) {
      if (e instanceof Error && e.message.includes("is not an admin resource")) throw new AdminConfigError(e.message);
      throw e;
    }
  }

  /** The permission names of the chosen strategy, `admin.access` first. They need a prepared registry, not a database. */
  get permissions(): string[] {
    return [...this.#site(this.registry).permissions];
  }

  /** Creates the names of `permissions` that the auth tables lack and reports the stale ones. It deletes nothing. */
  async syncPermissions(database: Database): Promise<SyncReport> {
    const site = this.#site(database.registry);
    try {
      return await grants.sync(site.permissions, site.labels, database, database.registry);
    } catch (e) {
      throw new AdminConfigError(e instanceof Error ? e.message : String(e));
    }
  }

  /** Writes the pattern grants of `roles` to the auth tables. A second run changes nothing; no role or grant is removed. Run `syncPermissions` first. */
  async seedRoles(database: Database): Promise<RoleSeedReport> {
    const site = this.#site(database.registry);
    try {
      return await grants.seed(this.#roles, site.permissions, database, database.registry);
    } catch (e) {
      throw new AdminConfigError(e instanceof Error ? e.message : String(e));
    }
  }

  /** `{ user }` is the in-memory path with no login; `{ secret }` runs the served router with login (see `AdminTestClient`). */
  testClient(database: Database, options: { readonly user?: AdminUser | null; readonly secret?: string; readonly settings?: Readonly<Record<string, unknown>>; readonly ip?: string } = {}): AdminTestClient {
    const { secret, settings, ip } = options;
    if (secret === undefined && (settings !== undefined || ip !== undefined)) throw new TypeError("testClient takes `settings` and `ip` only with `secret`");
    const login = secret === undefined ? null : { secret, ...(settings === undefined ? {} : { settings }), ...(ip === undefined ? {} : { ip }) };
    return new AdminTestClient(this.#site(database.registry), database, options.user ?? null, login, this.prefix, this.#host());
  }

  /** @internal What `monorm admin` runs: serves this admin on the socket of `settingsJson`; `secret` is `MONORM_ADMIN_SECRET`. */
  serve(database: Database, settingsJson: string, secret: string | null = null): Promise<NativeAdminServer> {
    const site = this.#site(database.registry);
    let started: Promise<NativeAdminServer>;
    try {
      started = site.serve(database.engine, settingsJson, hostDispatch(database, this.#host()), secret);
    } catch (e) {
      throw new AdminSettingsError(e instanceof Error ? e.message : String(e));
    }
    return wait(() => started);
  }
}

const defaultAdmin = new Admin();

export const admin = {
  register: (model: AnyModel, config?: ResourceConfig): void => defaultAdmin.register(model, config),
  check: (): string[] => defaultAdmin.check(),
  url: (model: AnyModel, key: RecordKey): string => defaultAdmin.url(model, key),
  testClient: (database: Database, options?: Parameters<Admin["testClient"]>[1]): AdminTestClient => defaultAdmin.testClient(database, options),
};
