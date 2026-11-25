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
import { randomUUID } from "node:crypto";
import { createReadStream, createWriteStream } from "node:fs";
import { resolve as resolvePath } from "node:path";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { HOST, authUserTable, hostDispatch, type HostFunction } from "./admin-host.js";
import * as grants from "./admin-permissions.js";
import { Reply, routeHost, Stream, type RouteFunction, type RouteRequest } from "./admin-routes.js";
import { action, actionDef, actionFunction, Context, download, InputDef, input, log, message, redirect, refresh, serviceFunction, viewHost, writerHost, type ActionDef, type ActionFunction, type ActionOptions, type ActionResult, type InlineChanges } from "./admin-write.js";
import { fieldAdapters, hasFieldAdapters } from "./adapters.js";
import type { RoleSeedReport, SyncReport } from "./admin-permissions.js";
import type { Database } from "./db.js";
import { ORMError } from "./errors.js";
import { asCondition, Column, Condition, Expression, IRContext, Ordering, PATH, RelationPath, resolveNullRule, Slot, SlotParams } from "./expr.js";
import type { ModelSpec } from "./meta.js";
import { registry as defaultRegistry, type ModelClass, type Registry } from "./model.js";
import { Prepared } from "./query.js";
import { call, native, wait, type NativeAdminConfigBuilder, type NativeAdminRouter, type NativeAdminServer, type NativeAdminSite } from "./native.js";

export const DEFAULT_BASE_PATH = "/admin";

/** The config names an unknown field, a list column through a to-many relation, or a bad value. */
export class AdminConfigError extends ORMError {
  override name = "AdminConfigError";
  /** Which rule failed: `config.<rule>`, such as `config.unknown_field`. */
  code = "config.invalid_value";

  constructor(message: string, code?: string) {
    super(message);
    if (code !== undefined) this.code = code;
  }
}

/** A wrong setting or a missing or short `MONORM_ADMIN_SECRET`: `monorm admin` exits with code 2. */
export class AdminSettingsError extends ORMError {
  override name = "AdminSettingsError";
  /** `settings.<rule>`, such as `settings.secret`. */
  code = "settings.invalid_value";

  constructor(message: string, code?: string) {
    super(message);
    if (code !== undefined) this.code = code;
  }
}

/** The error class of an error the native module threw: its `code` picks the class. */
export function adminError(e: unknown): AdminConfigError | AdminSettingsError {
  const code = e instanceof Error ? (e as { code?: unknown }).code : undefined;
  const message = e instanceof Error ? e.message : String(e);
  if (typeof code === "string" && code.startsWith("settings.")) return new AdminSettingsError(message, code);
  return new AdminConfigError(message, typeof code === "string" && code.startsWith("config") ? code : undefined);
}

/** Calls a native admin function and raises its refusal as an `AdminConfigError`. */
function adminCall<T>(f: () => T): T {
  try {
    return call(f);
  } catch (e) {
    throw e instanceof AdminConfigError ? e : adminError(e);
  }
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
  /** A sort header. It needs `orderBy`, or `expr` and `nullable: false`. */
  readonly sortable?: boolean;
  /**
   * What the sort header orders by: a field of the model or of a to-one relation (`Note.author.name`, not null), or an expression
   * (the column then declares `nullable: false`). It makes the column sortable, also for `function`.
   */
  readonly orderBy?: unknown;
}

export type BadgeColour = "grey" | "blue" | "green" | "yellow" | "red" | "purple";

export interface DateHierarchy {
  /** A date or datetime field, by path (`"author.joined"`) or column. One to-one hop is the most. */
  readonly field: AnyColumn | string;
  /** `false` skips the counts: the bar shows the periods of the date range and runs no count statement. */
  readonly counts?: boolean;
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
  /** A column, by path or name, to a map of value to colour: the list and the detail page show the value as a badge. A value outside the map shows grey. */
  readonly badges?: Readonly<Record<string, Readonly<Record<string, BadgeColour>>>>;
  /** A date or datetime field (one to-one hop allowed): a bar over the table drills down year, month and day. It joins `filters`. */
  readonly dateHierarchy?: AnyColumn | string | DateHierarchy;
  /** The to-many relations that the detail page shows as tabs. Without it: every one that leads to a resource. */
  readonly tabs?: readonly AnyRelation[];
  readonly order?: readonly (AnyColumn | Ordering<any, any>)[];
  /** Rows per page, 1 to 200 (25 by default). */
  readonly pageSize?: number;
  /** The list columns that a sort header may use. Without it every list column that can sort. */
  readonly sortable?: readonly AnyColumn[];
  /** List columns that a user edits in place and saves for several rows at once. Each is a plain field of the model that the update form may write. */
  readonly editable?: readonly AnyColumn[];
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

/** The look of the UI in three values. Each one is the same value as a `brand` key; set it in one place only. */
export interface ThemeConfig {
  /** The name in the header and the page title (same as `brand.name`). */
  readonly title?: string;
  /** The URL of the logo image: a path or an `https` URL. */
  readonly logo?: string;
  /** The brand colour, `#rgb` or `#rrggbb` (same as `brand.colors.primary`). The UI picks a readable text colour for it. */
  readonly color?: string;
}

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
  /** The colour of the environment label. `production` gives `danger`, `staging` `warning`, any other label `info`. */
  readonly environmentLevel?: "info" | "warning" | "danger";
}

function brandJson({ environmentLevel, ...rest }: BrandConfig): Record<string, unknown> {
  return environmentLevel === undefined ? rest : { ...rest, environment_level: environmentLevel };
}

export interface SwitcherService {
  /** Lower case letters, digits, `-` and `_`; unique. */
  readonly key: string;
  /** 1 to 40 characters. */
  readonly title: string;
  /** A path that starts with `/` or an `https` URL. Needed for every service but the current one. */
  readonly url?: string;
  /** A lucide icon name such as `server`. */
  readonly icon?: string;
  /** `#rgb` or `#rrggbb`. The colour of the current service sets the primary colour. */
  readonly color?: string;
}

/** Plain strings that your project fills from its own environment. An empty string is no value. */
export interface SwitcherBuild {
  readonly commit?: string;
  readonly builtAt?: string;
  readonly deployedAt?: string;
  readonly tree?: string;
}

/** The service switcher. The same table in `monorm.toml` (`[admin.switcher]`, `${NAME}` reads the environment) adds keys; a key set in both is an error. */
export interface SwitcherConfig {
  /** The `key` of the service that this admin is. */
  readonly current?: string;
  readonly services?: readonly SwitcherService[];
  readonly build?: SwitcherBuild;
}

function switcherJson({ build, ...rest }: SwitcherConfig): Record<string, unknown> {
  if (build === undefined) return rest;
  const { builtAt, deployedAt, ...plain } = build;
  return { ...rest, build: { ...plain, ...(builtAt === undefined ? {} : { built_at: builtAt }), ...(deployedAt === undefined ? {} : { deployed_at: deployedAt }) } };
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
  /** Package detail tabs, each `"<package>.<tab>"`: the detail page shows them after the relation tabs. See `Admin.package`. */
  readonly detailTabs?: readonly string[];
  readonly frames?: readonly FrameConfig[];
  readonly fieldsets?: readonly Fieldset[];
  /**
   * The relation fields that the form edits, by relation name. A to-one relation becomes a picker with keyset autocomplete;
   * a many-to-many relation (through a join model) becomes chips. The target of each needs a text column that names a row.
   */
  readonly relations?: Readonly<Record<string, RelationConfig>>;
  /** File fields by field name, each with the name of its storage in the `storage` registry of the admin (`{ cover: "media" }`). A file field is a nullable `Json` field that holds the reference of the file. */
  readonly files?: Readonly<Record<string, string>>;
  /** The service of a create: `async (ctx, values) => record`. It owns its transactions and returns the new record. */
  readonly create?: (ctx: Context, values: any) => Promise<unknown>;
  /** The service of an update: `async (ctx, qs, changes)`. `qs` is the one-row query set (with the `queryset` filter and the `can` rule). A `VersionConflict` is a 409. */
  readonly update?: (ctx: Context, qs: any, changes: any) => Promise<unknown>;
  /** The service of a delete: `async (ctx, qs)`. */
  readonly delete?: (ctx: Context, qs: any) => Promise<unknown>;
  /** Controls of the form that are no field (`input("reason", { type: "textarea" })`). Only a service reads them (`ctx.inputs`). */
  readonly inputs?: readonly InputDef[];
  /** Open application write rules for this resource. The audit row records the opt-in. */
  readonly allowProtectedWrites?: boolean;
  /**
   * To-many relations that the detail page shows and edits in place: `inline(OrderLine)` for each one (a bare model works too).
   * The inline takes the registered page of the child model; `inline(..., { page: {...} })` overrides page keys.
   */
  readonly inlines?: readonly (InlineDef | AnyModel)[];
  readonly list?: ListConfig;
  /** The name in the navigation and the page heading (the model name, spaced, by default). */
  readonly title?: string;
  /** The navigation group: one of `groups` of the admin. */
  readonly group?: string;
  /** A lucide icon name such as `book-open`, shown in the navigation. */
  readonly icon?: string;
  /** The operations that the resource allows for every user, superusers too. */
  readonly allow?: readonly ("read" | "create" | "update" | "delete")[];
  /** The `page` strategy only: add `"delete"` to split the delete permission from `manage`. */
  readonly perms?: readonly ("read" | "manage" | "delete")[];
  /** The label of the permission names of this resource, instead of the site label. */
  readonly label?: string;
  /** Names such as `"see_credential"` that become `<label>.<name>`. */
  readonly fieldPermissions?: readonly string[];
  /**
   * Field permissions that show a field: `{ cost: "see_cost" }` shows the column `cost` only to a user who holds `<label>.see_cost`.
   * Without it the field leaves the list, detail, tabs, search, filters, sort, export, labels, inlines and the response.
   * A filter, sort or write on it is an error.
   */
  readonly see?: Readonly<Record<string, string>>;
  /** Field permissions that change a field: `{ price: "change_price" }` makes `price` read-only for a user who does not hold `<label>.change_price`. */
  readonly change?: Readonly<Record<string, string>>;
  /**
   * The actions of the resource. A function that `action(fn, options)` marked is a button (see `ActionOptions`).
   * A plain name such as `"refund"` only declares the permission `<label>.<action>_<resource>`.
   */
  readonly actions?: readonly (string | ActionFunction)[];
  /** `false` removes the bulk delete of the list. It is on by default; it uses the `delete` service and the `can.delete` rule. */
  readonly bulkDelete?: boolean;
  /** Enable the built-in password action on the active auth user resource. */
  readonly setPassword?: boolean;
}

export interface RelationConfig {
  /** The text column of the target that names a row in the picker (the first text column by default). */
  readonly label?: string;
  /** Offer a modal in the form that creates a record of the target and fills the field. */
  readonly create?: boolean;
  /** A prepared query set of the target that limits the picker. The server checks it again on write. It combines with the `queryset` of the target. */
  readonly choices?: Prepared<any, any, any>;
}

/** The file storage of the admin: a `Registry` of `@ufstack/monorm/extensions/file-storage`. */
export interface FileStorage {
  upload(storage: string, source: AsyncIterable<Uint8Array>, options: { filename?: string; contentType?: string; maxSize?: number }): Promise<{ toJSON(): unknown }>;
  open(reference: any): AsyncIterable<Uint8Array>;
}

/** `page`: `<label>.read_<resource>` and `<label>.manage_<resource>` (the default); `model`: `add_`, `change_`, `delete_`, `view_`; `access`: only `admin.access`. */
export type PermissionStrategy = "page" | "model" | "access";

export interface FormSection {
  readonly title?: string;
  readonly fields: readonly string[];
  /** A collapsed section starts closed. It needs a `title`. */
  readonly collapsed?: boolean;
}

export interface FrameConfig {
  readonly name: string;
  readonly title: string;
  readonly url: string;
  readonly sandbox?: readonly string[];
  readonly scripts?: boolean;
  readonly sameOrigin?: boolean;
  readonly height?: string;
}

export interface Fieldset {
  readonly name: string;
  readonly fields: readonly string[];
  readonly title?: string;
}

export interface EmbedConfig {
  readonly resource: string;
  readonly tab: string;
  readonly lookup: string;
  readonly ancestors: readonly string[];
}

export interface PackageTabConfig {
  readonly module: string;
  readonly embeddable?: boolean;
}

function frameJson({ sameOrigin, ...frame }: FrameConfig): Record<string, unknown> {
  return { ...frame, same_origin: sameOrigin };
}

export interface AdminOptions {
  /** The URL path of the whole admin (`/admin` by default), for a gateway that passes the full path. The API prefix is `<basePath>/api` unless `prefix` sets another one under it. */
  readonly basePath?: string;
  /** The API prefix; it must sit under the base path. */
  readonly prefix?: string;
  readonly permissions?: PermissionStrategy;
  /** The label of the permission names (`admin` by default). */
  readonly label?: string;
  /** Role name to pattern grants (`{ support: ["orders.*", "-shops.see_credential"] }`) that `seedRoles` writes to the auth tables. */
  readonly roles?: Readonly<Record<string, readonly string[]>>;
  /** The name, logo, colors, header links and environment label of the UI. */
  readonly brand?: BrandConfig;
  /** The title, logo and brand colour of the UI in one small object (`{ title: "Acme", logo: "/static/acme.svg", color: "#0a7d5a" }`); it must not repeat a `brand` value. */
  readonly theme?: ThemeConfig;
  /** A field (`Book.title`), a nested subfield (`Geo.lat`) or a type name (`Text`, an enum, a nested type) to a widget for every resource; a resource `widgets` entry wins at one level. */
  readonly widgets?: Readonly<Record<string, string>>;
  /** The order of the navigation groups; a resource names one of them with `group`. */
  readonly groups?: readonly string[];
  /** A group to a lucide icon name for the navigation. */
  readonly groupIcons?: Readonly<Record<string, string>>;
  /** The services of your deploy and the build strings; `[admin.switcher]` of `monorm.toml` takes the same keys. */
  readonly switcher?: SwitcherConfig;
  readonly frameOrigins?: readonly string[];
  readonly frameCards?: readonly FrameConfig[];
  readonly embeds?: Readonly<Record<string, EmbedConfig>>;
  /** The time limit in seconds of each request's database statements (15 by default, 0 for none). */
  readonly statementTimeout?: number;
  /** The `Registry` of the file storage (`extensions/file-storage`) that holds the files of the `files` fields. */
  readonly storage?: FileStorage;
}

/** One key field, the key values in key order, or an object of key fields. */
type KeyPart = string | number | bigint | boolean | Date;
export type RecordKey = KeyPart | readonly KeyPart[] | { readonly [field: string]: KeyPart };

export type { RoleSeedReport, SyncReport };

export interface AdminUser {
  readonly is_superuser?: boolean;
  readonly [key: string]: unknown;
}

/** The `relations` option on the wire. Rust checks the names, the option keys and the value types. */
function relationsJson(name: string, cls: AnyModel, registry: Registry, value: unknown, fns: Fns): unknown {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return value;
  const out: Record<string, unknown> = {};
  for (const [relName, options] of Object.entries(value)) {
    const found = cls._meta.relations.get(relName);
    if (typeof options !== "object" || options === null || Array.isArray(options)) {
      out[relName] = options;
      continue;
    }
    const entry: Record<string, unknown> = { ...options };
    if ((options as RelationConfig).choices !== undefined) {
      const target = found === undefined ? cls : ({ _meta: registry.get(found.target) } as unknown as AnyModel);
      entry["choices"] = hostJson(target, "relations.choices", (options as RelationConfig).choices, "", fns);
    }
    out[relName] = entry;
  }
  return out;
}

function filesJson(name: string, value: unknown): Record<string, string> {
  if (typeof value !== "object" || value === null || Array.isArray(value) || !Object.values(value).every((v) => typeof v === "string" && v !== "")) {
    throw new AdminConfigError(`${name}: files maps a field name to a storage name`);
  }
  return { ...(value as Record<string, string>) };
}

/** The host functions `file.store` and `file.fetch`: the core hands over a temporary file, the registry moves the bytes. */
function fileHost(storage: FileStorage, files: ReadonlyMap<string, string>): Record<string, HostFunction> {
  return {
    async "file.store"(_database, args) {
      const name = files.get(`${args.model}.${args.field}`);
      if (name === undefined) throw new AdminConfigError(`${args.model}.${args.field} is not a file field`);
      const reference = await storage.upload(name, createReadStream(args.path), { filename: args.filename, contentType: args.content_type, maxSize: args.max_size });
      return reference.toJSON();
    },
    async "file.fetch"(_database, args) {
      await pipeline(Readable.from(storage.open(args.reference)), createWriteStream(args.path));
      return null;
    },
  };
}

/** A multipart body with one part named `file`. */
function multipartBody(file: UploadFile): { type: string; data: Buffer } {
  const [name, data, type] = file;
  const boundary = `----monorm${randomUUID().replaceAll("-", "")}`;
  const head = Buffer.from(`--${boundary}\r\nContent-Disposition: form-data; name="file"; filename="${name}"\r\nContent-Type: ${type}\r\n\r\n`);
  const tail = Buffer.from(`\r\n--${boundary}--\r\n`);
  return { type: `multipart/form-data; boundary=${boundary}`, data: Buffer.concat([head, typeof data === "string" ? Buffer.from(data) : data, tail]) };
}

/** The name of a host value's type, for a `$host` tag. */
function hostType(value: unknown): string {
  if (value === null) return "null";
  if (typeof value === "object" || typeof value === "function") return (value as { constructor?: { name?: string } }).constructor?.name || typeof value;
  return typeof value;
}

/** A column or relation of a model on the wire: a name stays, a column or relation is `$ref`, any other value is `$host`. Rust checks the model and the name. */
function refJson(ref: unknown): unknown {
  if (typeof ref === "string") return ref;
  if (ref instanceof Column) return { $ref: { model: ref.root.name, path: [...ref.path, ref.field.ir].join(".") } };
  const state = (ref as AnyRelation | undefined)?.[PATH];
  if (state !== undefined) return { $ref: { model: state.root.name, path: state.path.join(".") } };
  return { $host: hostType(ref) };
}

/** The engine IR of an expression or a condition, with literals inlined. `wire` sends it as `{ $ir }`, with a `slot` node for each `param(...)`; Rust checks the slots. */
function irOf(model: AnyModel, node: unknown, what: string, wire = false): unknown {
  const where = `${model._meta.name}: ${what}`;
  const params: unknown[] = wire ? new SlotParams() : [];
  let ir: unknown;
  try {
    const value = node instanceof Condition ? resolveNullRule(node) : node instanceof Expression ? node : asCondition(node);
    ir = (value as Expression<unknown>).ir(new IRContext(model._meta as never, params));
  } catch (e) {
    throw new AdminConfigError(`${where}: ${e instanceof Error ? e.message : String(e)}`);
  }
  const inlined = inlineParams(ir, params, where, wire);
  return wire ? { $ir: inlined } : inlined;
}

function inlineParams(ir: unknown, params: readonly unknown[], where: string, wire = false): unknown {
  if (Array.isArray(ir)) return ir.map((item) => inlineParams(item, params, where, wire));
  if (ir === null || typeof ir !== "object") return ir;
  const node = ir as Record<string, unknown>;
  if (node["t"] === "param") {
    const value = params[node["i"] as number];
    if (wire && value instanceof Slot) return { t: "slot", name: value.name, transform: value.transform !== undefined };
    if (value === null) return { t: "null" };
    if (typeof value === "boolean") return { t: "const", value };
    if (typeof value === "string") return { t: "text", value };
    if (typeof value === "number" && Number.isSafeInteger(value)) return { t: "int", value };
    // The IR goes as a JSON number, which keeps an integer exactly only up to 2^53 - 1.
    if (typeof value === "bigint" && value >= BigInt(Number.MIN_SAFE_INTEGER) && value <= BigInt(Number.MAX_SAFE_INTEGER)) return { t: "int", value: Number(value) };
    // A bigint out of range or a non-integer number is the user's integer; the limit comes from the JSON transport.
    if (typeof value === "bigint" || typeof value === "number") throw new AdminConfigError(`${where}: ${typeof value === "bigint" ? "a bigint" : "the number " + String(value)} is not a literal of the admin; use an integer inside the safe range (±2^53 - 1)`);
    if (wire) return { $host: hostType(value) };
    throw new AdminConfigError(`${where}: a value of this type is not a literal of the admin; use a safe integer, a string or a boolean`);
  }
  return Object.fromEntries(Object.entries(node).map(([k, v]) => [k, inlineParams(v, params, where, wire)]));
}

/** An order item on the wire: the column, with `desc` and `nulls` beside it. Rust refuses an expression and a placement. */
function orderJson(model: AnyModel, item: unknown): unknown {
  const ordering = item instanceof Ordering ? item : undefined;
  const expr = ordering === undefined ? item : ordering.expr;
  const by = expr instanceof Expression && !(expr instanceof Column) ? irOf(model, expr, "order", true) : refJson(expr);
  const desc = ordering?.descending === true;
  const nulls = ordering?.nulls;
  if (typeof by !== "object" || by === null || (!desc && nulls === undefined)) return by;
  return { ...by, ...(desc ? { desc: true } : {}), ...(nulls !== undefined ? { nulls } : {}) };
}

/** The config positions that hold a host object, and what each holds. The call sites send each one through `hostJson`. */
const HOST_FIELDS: Readonly<Record<string, "column" | "relation" | "order" | "order_by" | "ir" | "queryset" | "function">> = {
  "list.columns": "column",
  "list.filters": "column",
  "list.tabs": "column",
  "list.editable": "column",
  "list.sortable": "column",
  "list.search": "column",
  "list.dateHierarchy": "column",
  "list.dateHierarchy.field": "column",
  "list.order": "order",
  "list.aggregates.relation": "relation",
  "list.aggregates.field": "column",
  "list.computed.orderBy": "order_by",
  "list.computed.expr": "ir",
  "list.namedFilters.choices.condition": "ir",
  "list.searchWith": "function",
  "list.computed.function": "function",
  can: "ir",
  "actions.can": "ir",
  "actions.appliesTo": "ir",
  queryset: "queryset",
  "relations.choices": "queryset",
};

/** The wire form of the value at config position `path`, by its kind in `HOST_FIELDS`. */
function hostJson(model: AnyModel, path: string, value: unknown, what = "", fns?: Fns, maker?: () => HostFunction): unknown {
  switch (HOST_FIELDS[path]) {
    case "column":
      return refJson(value);
    case "relation":
      return typeof value === "string" || (value as AnyRelation | undefined)?.[PATH] !== undefined ? refJson(value) : { $host: hostType(value) };
    case "order":
      return orderJson(model, value);
    case "order_by":
      return value instanceof Column ? refJson(value) : value instanceof Expression || value instanceof Condition ? irOf(model, value, what, true) : { $host: hostType(value) };
    case "ir":
      return irOf(model, value, what, true);
    case "queryset":
      return querysetJson(model, value as NonNullable<ResourceConfig["queryset"]>, fns!);
    default:
      return fns!.register(maker!);
  }
}

/** The host functions of one config: a local id for each, sent as `{ $fn: id }`. Rust names them and returns the table. */
class Fns {
  readonly #makers = new Map<string, () => HostFunction>();

  register(maker: () => HostFunction): { $fn: string } {
    const key = `f${this.#makers.size}`;
    this.#makers.set(key, maker);
    return { $fn: key };
  }

  host(handles: Readonly<Record<string, string>>): Record<string, HostFunction> {
    return Object.fromEntries(Object.entries(handles).map(([key, name]) => [name, this.#makers.get(key)!()]));
  }
}

function dateHierarchyJson(model: AnyModel, value: AnyColumn | string | DateHierarchy): unknown {
  if (typeof value === "string" || value instanceof Column) return hostJson(model, "list.dateHierarchy", value);
  const { field, ...rest } = value as DateHierarchy;
  return { ...rest, ...(field === undefined ? {} : { field: hostJson(model, "list.dateHierarchy.field", field) }) };
}

function computedJson(model: AnyModel, items: readonly ComputedColumn[], fns: Fns): unknown[] {
  return items.map((item) => {
    const { expr, function: fn, orderBy, ...rest } = item;
    return {
      ...rest,
      ...(orderBy !== undefined ? { order_by: hostJson(model, "list.computed.orderBy", orderBy, `computed '${item.name}' order_by`) } : {}),
      ...(expr !== undefined ? { expr: hostJson(model, "list.computed.expr", expr, `computed '${item.name}'`) } : {}),
      ...(fn !== undefined ? { function: hostJson(model, "list.computed.function", fn, "", fns, () => batchHost(model, fn)) } : {}),
    };
  });
}

function aggregatesJson(model: AnyModel, items: readonly AggregateColumn[]): unknown[] {
  return items.map((item) => {
    const out: Record<string, unknown> = { name: item.name, relation: hostJson(model, "list.aggregates.relation", item.relation), fn: item.fn };
    if (item.label !== undefined) out["label"] = item.label;
    if (item.field !== undefined) out["field"] = hostJson(model, "list.aggregates.field", item.field);
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
      expr: hostJson(model, "list.namedFilters.choices.condition", c.condition, `named filter '${item.name}'`),
    })),
  }));
}

/** The queryset on the wire: `$fn` for a function, `$qs` for a prepared query set. Rust checks both. */
function querysetJson(model: AnyModel, queryset: NonNullable<ResourceConfig["queryset"]>, fns: Fns): unknown {
  const name = model._meta.name;
  if (typeof queryset === "function") return fns.register(() => querysetHost(model, queryset));
  if (!(queryset instanceof Prepared)) return { $host: hostType(queryset) };
  const qs = (queryset as any)["qs"];
  const state = qs.state;
  const body: Record<string, unknown> = {
    model: qs.meta.name,
    load: Boolean(state.loads?.length) || state.modelFields !== undefined || state.related.length > 0 || state.prefetch.length > 0,
    slice: state.limit !== undefined || state.offset !== undefined,
    join: state.from !== undefined || state.joins.length > 0,
    lock: state.lock !== undefined,
    without_defaults: state.withoutDefaults !== false,
  };
  // Rust refuses these with its code; building the filter first would raise a binding text before it.
  if (qs.meta.name !== name || state.filters.length === 0 || ["load", "slice", "join", "lock", "without_defaults"].some((flag) => body[flag])) return { $qs: body };
  const where = `${name}: queryset`;
  const params = new SlotParams();
  let irs: unknown[];
  try {
    const ctx = new IRContext(model._meta as never, params);
    irs = state.filters.map((f: Condition) => inlineParams((resolveNullRule(f) as Expression<unknown>).ir(ctx), params, where, true));
  } catch (e) {
    if (e instanceof AdminConfigError) throw e;
    throw new AdminConfigError(`${where}: ${e instanceof Error ? e.message : String(e)}`);
  }
  body["filter"] = { $ir: irs.length === 1 ? irs[0] : { t: "and", items: irs } };
  body["order"] = (state.order as Ordering<any, any>[]).map((o) => orderJson(model, o));
  return { $qs: body };
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

/** The action entries of a resource: a name declares a permission, a marked function is a button. */
function actionsJson(name: string, model: AnyModel, items: unknown, fnEntry: (d: ActionDef) => Record<string, unknown>): Record<string, unknown>[] {
  if (!Array.isArray(items)) throw new AdminConfigError(`${name}: actions is a list`);
  const out: Record<string, unknown>[] = [];
  for (const item of items) {
    const d = actionDef(item);
    let entry: Record<string, unknown>;
    if (d === undefined) {
      if (typeof item !== "string") throw new AdminConfigError(`${name}: an action is a name or a function that action() marked, not ${String(item)}`);
      entry = { name: item, permission: item };
    } else {
      const placement = d.placement ?? "bulk";
      if (d.name === "") throw new AdminConfigError(`${name}: an action needs a name: pass { name } to action()`);
      if (d.inputs !== undefined && !d.inputs.every((i) => i instanceof InputDef)) throw new AdminConfigError(`${name}: action '${d.name}': inputs is a list of input(...) controls`);
      entry = { name: d.name, placement, ...fnEntry(d) };
      if (d.permission !== undefined) entry["permission"] = d.permission;
      if (d.label !== undefined) entry["label"] = d.label;
      if (d.confirm !== undefined) entry["confirm"] = d.confirm;
      if (d.inputs !== undefined && d.inputs.length > 0) entry["inputs"] = d.inputs.map((i) => i.json());
      if (d.appliesTo !== undefined) entry["applies_to"] = hostJson(model, "actions.appliesTo", d.appliesTo, `action '${d.name}' appliesTo`);
      if (d.can !== undefined) entry["can"] = hostJson(model, "actions.can", d.can, `action '${d.name}' can`);
      if (d.replaceChange !== undefined) entry["replace_change"] = d.replaceChange;
    }
    out.push(entry);
  }
  return out;
}

function canJson(model: AnyModel, rules: CanRules): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [rule, condition] of Object.entries(rules)) {
    if (condition !== undefined) out[rule] = hostJson(model, "can", condition, `can '${rule}'`);
  }
  return out;
}

function listJson(model: AnyModel, config: ListConfig, fns: Fns): Record<string, unknown> {
  // An unknown key goes as null: Rust names it.
  const out: Record<string, unknown> = Object.fromEntries(Object.keys(config).map((k) => [k.replace(/[A-Z]/g, (c) => `_${c.toLowerCase()}`), null]));
  if (config.columns !== undefined) out["columns"] = config.columns.map((c) => hostJson(model, "list.columns", c));
  if (config.filters !== undefined) out["filters"] = config.filters.map((c) => hostJson(model, "list.filters", c));
  if (config.tabs !== undefined) out["tabs"] = config.tabs.map((c) => hostJson(model, "list.tabs", c));
  if (config.editable !== undefined) out["editable"] = config.editable.map((c) => hostJson(model, "list.editable", c));
  if (config.namedFilters !== undefined) out["named_filters"] = namedJson(model, config.namedFilters);
  if (config.computed !== undefined) out["computed"] = computedJson(model, config.computed, fns);
  if (config.aggregates !== undefined) out["aggregates"] = aggregatesJson(model, config.aggregates);
  if (config.labels !== undefined) out["labels"] = { ...config.labels };
  if (config.empty !== undefined) out["empty"] = config.empty;
  if (config.emptyFields !== undefined) out["empty_fields"] = { ...config.emptyFields };
  if (config.dateHierarchy !== undefined) out["date_hierarchy"] = dateHierarchyJson(model, config.dateHierarchy);
  if (config.badges !== undefined) {
    out["badges"] = Object.fromEntries(Object.entries(config.badges).map(([column, map]) => [column, { ...map }]));
  }
  if (config.order !== undefined) out["order"] = config.order.map((item) => hostJson(model, "list.order", item));
  if (config.pageSize !== undefined) out["page_size"] = config.pageSize;
  if (config.sortable !== undefined) out["sortable"] = config.sortable.map((c) => hostJson(model, "list.sortable", c));
  if (config.search !== undefined) out["search"] = config.search.map((c) => hostJson(model, "list.search", c));
  if (config.searchWith !== undefined) out["search_with"] = hostJson(model, "list.searchWith", config.searchWith, "", fns, () => searchHost(model, config.searchWith!));
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
    /** The body as bytes: a file download is not text. Login mode keeps only the UTF-8 text. */
    readonly content: Uint8Array = new TextEncoder().encode(text),
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
const CSRF_COOKIE = "__Secure-monorm-csrf";

export interface RequestOptions {
  /** The JSON body. */
  readonly json?: unknown;
  readonly headers?: Readonly<Record<string, string>>;
  /** The CSRF token of a write: the jar's token unless given; `null` leaves the header out. */
  readonly csrf?: string | null;
  /** The `Origin` of a write: `http://localhost` unless given; `null` leaves the header out. */
  readonly origin?: string | null;
  /** A multipart upload: `[filename, data, contentType]`. Login mode sends text files only (UTF-8). */
  readonly file?: UploadFile;
}

export type UploadFile = readonly [filename: string, data: string | Uint8Array, contentType: string];

export interface LoginOptions {
  /** The signing secret (`MONORM_ADMIN_SECRET`): at least 32 bytes. */
  readonly secret: string;
  /** `idle_timeout` and `session_timeout` (whole seconds above 0), `origins`, `trusted_proxies`, `base_path` and `oidc` (`issuer`, `client_id`, `client_secret`, `roles_claim`, `scopes`, `hd`, `auto_start`). */
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
    private readonly host: Readonly<Record<string, HostFunction>> = HOST,
  ) {
    if (login !== null && user !== null) throw new TypeError("testClient takes `user` (no login) or `secret` (login), not both");
    this.#user = user === null ? null : JSON.stringify(user);
    this.#login = login;
    this.ip = login?.ip ?? "127.0.0.1";
  }

  /** The API prefix of the site: the paths of a request start with it. */
  private get prefix(): string {
    return this.site.prefix;
  }

  /** Moves the clock of the login router forward: a test ends a session or a renewal without a wait. Send a request first. */
  advanceClock(seconds: number): void {
    if (this.#router === null) throw new Error("advanceClock needs a login-mode client that has sent a request");
    this.#router.advanceClock(seconds);
  }

  async request(method: string, path: string, options: RequestOptions = {}): Promise<AdminResponse> {
    const login = this.#login;
    if (login === null) {
      if (Object.keys(options).some((k) => k !== "json" && k !== "file")) throw new TypeError("the `user` client takes only a method, a path, `json` and `file`");
      if (options.file !== undefined && options.json !== undefined) throw new TypeError("a request takes `json` or `file`, not both");
      const trace = new (native().Trace)();
      if (options.file !== undefined || this.site.isRaw(path)) {
        const part = options.file !== undefined ? multipartBody(options.file) : options.json !== undefined ? { type: "application/json", data: Buffer.from(JSON.stringify(options.json)) } : null;
        const raw = await wait(() => this.site.requestRaw(this.database.engine, method, path, this.#user, trace, hostDispatch(this.database, this.host), part?.type ?? null, part?.data ?? null));
        const events = trace.take();
        const body = new Uint8Array(raw.body);
        return new AdminResponse(raw.status, new TextDecoder().decode(body), events.length, raw.headers as [string, string][], events.map((e) => e.sql), body);
      }
      const sentJson = options.json === undefined ? null : JSON.stringify(options.json);
      const { status, body } = await wait(() => this.site.request(this.database.engine, method, path, this.#user, trace, hostDispatch(this.database, this.host), sentJson));
      const events = trace.take();
      return new AdminResponse(status, body, events.length, [], events.map((e) => e.sql));
    }
    try {
      this.#router ??= call(() =>
        this.site.client(this.database.engine, hostDispatch(this.database, this.host), login.secret, JSON.stringify(login.settings ?? {}), null),
      );
    } catch (e) {
      const code = e instanceof Error ? (e as { code?: unknown }).code : undefined;
      throw typeof code === "string" && /^(config|settings)(\.|$)/.test(code) ? adminError(e) : e;
    }
    const sent = new Map<string, string>([["host", "localhost"]]);
    for (const [k, v] of Object.entries(options.headers ?? {})) sent.set(k.toLowerCase(), v);
    if (this.cookies.size > 0) sent.set("cookie", [...this.cookies].map(([k, v]) => `${k}=${v}`).join("; "));
    if (!["GET", "HEAD", "OPTIONS"].includes(method)) {
      const csrf = options.csrf === undefined ? (this.cookies.get(CSRF_COOKIE) ?? "") : options.csrf;
      if (csrf !== null) sent.set("x-csrf-token", csrf);
      const origin = options.origin === undefined ? "http://localhost" : options.origin;
      if (origin !== null) sent.set("origin", origin);
    }
    let body = options.json === undefined ? null : JSON.stringify(options.json);
    if (body !== null && !sent.has("content-type")) sent.set("content-type", "application/json");
    if (options.file !== undefined) {
      const part = multipartBody(options.file);
      try {
        body = new TextDecoder("utf-8", { fatal: true }).decode(part.data);
      } catch {
        throw new TypeError("the login-mode client sends text files only (UTF-8); use `user` for binary data");
      }
      if (!sent.has("content-type")) sent.set("content-type", part.type);
    }
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

export type InlineOptions = {
  /** The relation of the parent when more than one leads to the model. */
  readonly via?: string;
  /** `with_parent` (the default) sends the rows with the parent in one change set and one transaction; `row` saves each row alone through the page of the child. */
  readonly save?: "with_parent" | "row";
  /** A name (`"edit_lines"` becomes `<label>.edit_lines`) that replaces the write permission of the parent. */
  readonly permission?: string;
  readonly style?: "table" | "stacked";
  /** The row cap, 1 to 200 (20 by default). */
  readonly max?: number;
  readonly label?: string;
  readonly collapsed?: boolean;
  readonly delete?: boolean;
  /** Page keys of the child that this inline overrides (`queryset`, `can`, `list`, `form`, `readonly`, `widgets`, `relations`, `inputs`, `checkMessages`); a model with no page takes this as its page. */
  readonly page?: ResourceConfig;
};

/** A to-many relation that the detail page of the parent edits in place; see `inline`. */
export class InlineDef {
  readonly via: string | undefined;
  readonly save: "with_parent" | "row";
  readonly permission: string | undefined;
  readonly style: "table" | "stacked";
  readonly max: number;
  readonly label: string | undefined;
  readonly collapsed: boolean;
  readonly delete: boolean;
  readonly page: ResourceConfig;

  constructor(
    readonly model: AnyModel,
    options: InlineOptions = {},
  ) {
    const name = model._meta.name;
    this.save = options.save ?? "with_parent";
    this.style = options.style ?? "table";
    adminCall(() => native().AdminConfigBuilder.checkInline(name, this.save, this.style));
    this.via = options.via;
    this.permission = options.permission;
    this.max = options.max ?? 20;
    this.label = options.label;
    this.collapsed = options.collapsed ?? false;
    this.delete = options.delete ?? true;
    this.page = { ...(options.page ?? {}) };
  }
}

/**
 * The inline of a to-many relation: the registered page of `model` (or `page`, for a model with no page) as rows of the parent.
 * Put it in the `inlines` of the page of the parent.
 */
export function inline(model: AnyModel, options: InlineOptions = {}): InlineDef {
  return new InlineDef(model, options);
}

function inlineDefs(name: string, items: unknown): InlineDef[] {
  if (!Array.isArray(items)) throw new AdminConfigError(`${name}: inlines is a list of inline(...) wrappers or models`);
  return items.map((item) => {
    if (item instanceof InlineDef) return item;
    if (typeof item === "function" && (item as any)._meta !== undefined) return new InlineDef(item as AnyModel);
    throw new AdminConfigError(`${name}: inlines is a list of inline(...) wrappers or models`);
  });
}

/** The resources of one registry. */
export class Admin {
  prefix: string | undefined;
  basePath: string;
  readonly #resources = new Map<string, { model: AnyModel; config: ResourceConfig }>();
  readonly #strategy: PermissionStrategy;
  readonly #label: string;
  readonly #roles: Record<string, string[]>;
  readonly #widgets: Record<string, string> | undefined;
  readonly #groups: string[];
  readonly #groupIcons: Readonly<Record<string, string>> | undefined;
  /** The `[admin.switcher]` table of `monorm.toml`, set by the serve mode. */
  switcherSettings: unknown;
  readonly #frameOrigins: readonly string[] | undefined;
  readonly #frameCards: readonly FrameConfig[] | undefined;
  readonly #embeds: Readonly<Record<string, EmbedConfig>> | undefined;
  readonly #builder: NativeAdminConfigBuilder;
  readonly #storage: FileStorage | undefined;
  readonly #routes = new Map<string, { method: string; path: string; fn: RouteFunction; permission: string | undefined }>();
  #fns = new Fns();
  #planned: { schema: unknown; site: NativeAdminSite } | null = null;

  constructor(
    private readonly registry: Registry = defaultRegistry,
    options: AdminOptions = {},
  ) {
    this.#storage = options.storage;
    this.basePath = options.basePath ?? DEFAULT_BASE_PATH;
    this.prefix = options.prefix;
    this.#strategy = options.permissions ?? "page";
    this.#label = options.label ?? "admin";
    this.#widgets = options.widgets === undefined ? undefined : structuredClone(options.widgets);
    this.#groups = [...(options.groups ?? [])];
    this.#groupIcons = options.groupIcons;
    this.#frameOrigins = options.frameOrigins;
    this.#frameCards = options.frameCards;
    this.#embeds = options.embeds;
    const eager = {
      ...(options.brand === undefined ? {} : { brand: brandJson(options.brand) }),
      ...(options.theme === undefined ? {} : { theme: options.theme }),
      ...(options.switcher === undefined ? {} : { switcher: switcherJson(options.switcher) }),
    };
    this.#builder = adminCall(() => new (native().AdminConfigBuilder)(JSON.stringify(eager), options.statementTimeout ?? 15, JSON.stringify(options.roles ?? {}), "camel"));
    this.#roles = Object.fromEntries(Object.entries(options.roles ?? {}).map(([name, patterns]) => [name, [...patterns]]));
  }

  /** Makes `model` a resource. Once any model is registered, only registered models are resources. */
  register(model: AnyModel, config: ResourceConfig = {}): void {
    const name = model._meta.name;
    adminCall(() => this.#builder.register(name, Object.keys(config)));
    this.#resources.set(name, { model, config });
    this.#planned = null;
  }

  /**
   * Adds an extension package: a directory of static `.js`, `.mjs` and `.css` files that the admin serves to the browser.
   * `widgets` maps a widget name to a module file of the directory; a field uses it as `"<name>.<widget>"` in `widgets`.
   * `forms` does the same for a form module (a resource `formComponent` of `"<name>.<form>"`). `tabs` maps a tab name to a module: a resource shows it on its detail page with `detailTabs: ["<name>.<tab>"]`. `styles` lists the CSS files that
   * load with the first widget. `adminMajor` is the widget contract version that the files were built for (default 1).
   * The admin reads the files once and refuses a package for another major version. A package adds its routes and actions
   * with `route` and `action` itself: it is a function `install(admin)` that calls this method too.
   */
  package(name: string, directory: string, options: { readonly widgets?: Readonly<Record<string, string>>; readonly forms?: Readonly<Record<string, string>>; readonly tabs?: Readonly<Record<string, string | PackageTabConfig>>; readonly styles?: readonly string[]; readonly adminMajor?: number } = {}): void {
    const entry = { name, admin_major: options.adminMajor ?? 1, dir: resolvePath(directory), widgets: structuredClone(options.widgets ?? {}), forms: structuredClone(options.forms ?? {}), tabs: structuredClone(options.tabs ?? {}), styles: [...(options.styles ?? [])] };
    adminCall(() => this.#builder.package(name, JSON.stringify(entry)));
    this.#planned = null;
  }

  /**
   * Adds a custom route at `<prefix>/routes/<path>`. Rust runs the session, the CSRF check and the permission check, then calls
   * `fn(ctx, request)`. `path` may hold `{name}` parts (`request.params`). `permission` is a full `<label>.<name>`; without it the
   * route needs only the admin gate. `fn` returns a JSON value, `new Reply(json, { status })` or `new Stream(chunks, { contentType, filename })`.
   */
  route(method: string, path: string, fn: RouteFunction, options: { readonly permission?: string } = {}): void {
    adminCall(() => this.#builder.route(method, path, options.permission ?? null, typeof fn === "function"));
    const verb = method.toUpperCase();
    const clean = path.replace(/^\//, "");
    const id = `${verb} ${clean}`;
    this.#routes.set(id, { method: verb, path: clean, fn, permission: options.permission });
    this.#planned = null;
  }

  /** The part of the config that the host lowers at each build. The builder holds the rest. */
  #lateJson(registry: Registry): string {
    const fns = (this.#fns = new Fns());
    const resources = [...this.#resources].map(([model, { model: cls, config }]) => this.#resourceJson(model, cls, config, registry, fns));
    return JSON.stringify({
      base_path: this.basePath,
      ...(this.prefix === undefined ? {} : { prefix: this.prefix }),
      permissions: this.#strategy,
      label: this.#label,
      resources,
      ...(this.#storage === undefined ? {} : { storage: true }),
      ...(resources.some((r) => r["set_password"]) ? { user_table: authUserTable(registry) } : {}),
      ...(this.#widgets === undefined ? {} : { widgets: this.#widgets }),
      ...(this.#groups.length === 0 ? {} : { groups: this.#groups }),
      ...(this.#groupIcons === undefined || Object.keys(this.#groupIcons).length === 0 ? {} : { group_icons: this.#groupIcons }),
      ...(this.switcherSettings ? { switcher_file: this.switcherSettings } : {}),
      frame_origins: this.#frameOrigins,
      frame_cards: this.#frameCards?.map(frameJson),
      embeds: this.#embeds,
    });
  }

  /** The config JSON of one page. */
  #resourceJson(model: string, cls: AnyModel, config: ResourceConfig, registry: Registry, fns: Fns, top = true): Record<string, unknown> {
    const queryset = config.queryset === undefined ? undefined : hostJson(cls, "queryset", config.queryset, "", fns);
    const list = config.list === undefined ? undefined : listJson(cls, config.list, fns);
    return {
      model,
      ...(queryset === undefined ? {} : { queryset }),
      ...(config.can === undefined ? {} : { can: canJson(cls, config.can) }),
      // These values go as the user wrote them: Rust checks their shape and names the key in the error.
      ...(config.checkMessages === undefined ? {} : { check_messages: config.checkMessages }),
      ...(config.form === undefined ? {} : { form: config.form }),
      ...(config.readonly === undefined ? {} : { readonly: config.readonly }),
      ...(config.widgets === undefined ? {} : { widgets: config.widgets }),
      ...(config.formComponent === undefined ? {} : { form_component: config.formComponent }),
      ...(config.detailTabs === undefined ? {} : { detail_tabs: config.detailTabs }),
      frames: config.frames?.map(frameJson),
      fieldsets: config.fieldsets,
      ...(config.relations === undefined ? {} : { relations: relationsJson(model, cls, registry, config.relations, fns) }),
      ...(config.files === undefined ? {} : { files: Object.keys(filesJson(model, config.files)) }),
      ...(list === undefined ? {} : { list }),
      ...this.#writesJson(model, cls, config, fns, top),
      ...(config.title === undefined ? {} : { title: config.title }),
      ...(config.group === undefined ? {} : { group: config.group }),
      ...(config.icon === undefined ? {} : { icon: config.icon }),
      ...(config.allow === undefined ? {} : { allow: [...config.allow] }),
      ...(config.perms === undefined ? {} : { perms: [...config.perms] }),
      ...(config.label === undefined ? {} : { label: config.label }),
      ...(config.fieldPermissions === undefined ? {} : { field_permissions: [...config.fieldPermissions] }),
      ...(config.see === undefined ? {} : { see: config.see }),
      ...(config.change === undefined ? {} : { change: config.change }),
      ...(config.actions === undefined ? {} : { actions: actionsJson(model, cls, config.actions, (d) => this.#fnEntry(fns, d.fn, top, () => actionFunction(cls, config, d, this.#inlineModels()), true)) }),
      ...(config.setPassword === undefined ? {} : { set_password: config.setPassword }),
      ...(config.bulkDelete === undefined ? {} : { bulk_delete: config.bulkDelete }),
      ...(config.inlines === undefined ? {} : { inlines: inlineDefs(model, config.inlines).map((d) => this.#inlineJson(model, d, registry, fns)) }),
    };
  }

  #inlineJson(parent: string, d: InlineDef, registry: Registry, fns: Fns): Record<string, unknown> {
    const child = d.model._meta.name;
    const out: Record<string, unknown> = { model: child, save: d.save, style: d.style, max: d.max, collapsed: d.collapsed, delete: d.delete };
    if (d.via !== undefined) out["via"] = d.via;
    if (d.permission !== undefined) out["permission"] = d.permission;
    if (d.label !== undefined) out["label"] = d.label;
    const page = this.#resourceJson(child, d.model, d.page, registry, fns, false);
    // A page key that did not lower reaches Rust as null, so that Rust refuses it by name.
    for (const key of Object.keys(d.page)) page[key.replace(/[A-Z]/g, (c) => `_${c.toLowerCase()}`)] ??= null;
    if (Object.keys(page).length > 1) out["page"] = page;
    return out;
  }

  #inlineModels(): Map<string, AnyModel> {
    const out = new Map<string, AnyModel>();
    for (const [parent, { config }] of this.#resources) {
      for (const d of inlineDefs(parent, config.inlines ?? [])) out.set(d.model._meta.name, d.model);
    }
    return out;
  }

  /** A service or action function on the wire. A resource sends `$fn` with the two flags beside it; an inline page sends the flags alone. A value that is not callable is `$host`: Rust refuses it. */
  #fnEntry(fns: Fns, fn: unknown, top: boolean, maker: () => HostFunction, wrap = false): Record<string, unknown> {
    const entry = typeof fn !== "function" ? { $host: hostType(fn) } : top ? { ...fns.register(maker), sync: false, threaded: false } : { sync: false, threaded: false };
    return wrap && ("$fn" in entry || "$host" in entry) ? { function: entry } : entry;
  }

  #writesJson(name: string, cls: AnyModel, config: ResourceConfig, fns: Fns, top: boolean): Record<string, unknown> {
    const out: Record<string, unknown> = {};
    const services: Record<string, unknown> = {};
    for (const op of ["create", "update", "delete"] as const) {
      const fn = config[op];
      if (fn === undefined) continue;
      services[op] = this.#fnEntry(fns, fn, top, () => serviceFunction(cls, op, fn as never, this.#inlineModels()));
    }
    if (Object.keys(services).length > 0) out["service"] = services;
    if (config.inputs !== undefined) {
      if (!Array.isArray(config.inputs) || !config.inputs.every((d) => d instanceof InputDef)) throw new AdminConfigError(`${name}: inputs is a list of input(...) controls`);
      out["inputs"] = config.inputs.map((d) => d.json());
    }
    if (config.allowProtectedWrites === true) out["allow_protected_writes"] = true;
    if (hasFieldAdapters(cls._meta)) {
      out["host_behavior"] = true;
      const views = Object.fromEntries([...fieldAdapters(cls._meta)].flatMap(([field, adapter]) => (adapter.adminMode === undefined ? [] : [[cls._meta.fieldList.find((f) => f.name === field)?.ir ?? field, adapter.adminMode()] as const])));
      if (Object.keys(views).length > 0) out["views"] = views;
    }
    return out;
  }

  /** The host functions: login, the functions of the config under the names that Rust gave them, and the writes, services, actions, files and routes. */
  #host(handles: Readonly<Record<string, string>>): Record<string, HostFunction> {
    const host: Record<string, HostFunction> = { ...HOST, ...this.#fns.host(handles) };
    const models = this.#inlineModels();
    host["admin.write"] = writerHost(this.#resources, models);
    host["admin.view"] = viewHost(new Map([...models, ...[...this.#resources.values()].map(({ model }) => [model._meta.name, model] as const)]));
    const files = new Map<string, string>();
    for (const [name, { config }] of this.#resources) {
      for (const [field, storage] of Object.entries(filesJson(name, config.files ?? {}))) files.set(`${name}.${field}`, storage);
    }
    if (files.size > 0 && this.#storage !== undefined) Object.assign(host, fileHost(this.#storage, files));
    if (this.#routes.size > 0) Object.assign(host, routeHost([...this.#routes.values()].map((r) => [r.method, r.path, r.fn] as const)));
    return host;
  }

  #site(registry: Registry): NativeAdminSite {
    const schema = registry.native();
    try {
      return adminCall(() => this.#builder.build(schema, this.#lateJson(registry)));
    } catch (e) {
      if (e instanceof AdminConfigError) throw e;
      throw adminError(e);
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
   * The path of the page of one record: `<basePath>/<Model>/<key>`. `key` is the value of a single key field, the key values in
   * key order, or an object of key fields. Throws `AdminConfigError` when `model` is no resource and `Error` when the key does not fit.
   */
  url(model: AnyModel, key: RecordKey): string {
    const schema = this.registry.native();
    if (this.#planned === null || this.#planned.schema !== schema) this.#planned = { schema, site: this.#site(this.registry) };
    try {
      return call(() => this.#planned!.site.url(model._meta.name, JSON.stringify(keyJson(key))));
    } catch (e) {
      if (e instanceof Error && e.message.includes("is not an admin resource")) throw adminError(e);
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
      throw adminError(e);
    }
  }

  /** Writes the pattern grants of `roles` to the auth tables. A second run changes nothing; no role or grant is removed. Run `syncPermissions` first. */
  async seedRoles(database: Database): Promise<RoleSeedReport> {
    const site = this.#site(database.registry);
    try {
      return await grants.seed(this.#roles, site.permissions, database, database.registry);
    } catch (e) {
      throw adminError(e);
    }
  }

  /** `{ user }` is the in-memory path with no login; `{ secret }` runs the served router with login (see `AdminTestClient`). */
  testClient(database: Database, options: { readonly user?: AdminUser | null; readonly secret?: string; readonly settings?: Readonly<Record<string, unknown>>; readonly ip?: string } = {}): AdminTestClient {
    const { secret, settings, ip } = options;
    if (secret === undefined && (settings !== undefined || ip !== undefined)) throw new TypeError("testClient takes `settings` and `ip` only with `secret`");
    const login = secret === undefined ? null : { secret, ...(settings === undefined ? {} : { settings }), ...(ip === undefined ? {} : { ip }) };
    const site = this.#site(database.registry);
    return new AdminTestClient(site, database, options.user ?? null, login, this.#host(site.handles));
  }

  /** @internal What `monorm admin` runs: serves this admin on the socket of `settingsJson`; `secret` is `MONORM_ADMIN_SECRET`. */
  serve(database: Database, settingsJson: string, secret: string | null = null): Promise<NativeAdminServer> {
    const site = this.#site(database.registry);
    let started: Promise<NativeAdminServer>;
    try {
      started = site.serve(database.engine, settingsJson, hostDispatch(database, this.#host(site.handles)), secret);
    } catch (e) {
      const err = adminError(e);
      throw err instanceof AdminSettingsError ? err : new AdminSettingsError(err.message);
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
  input,
  log,
  inline,
  action,
  message,
  redirect,
  download,
  refresh,
};

export { action, Context, download, InputDef, input, log, message, redirect, refresh, Reply, Stream };
export type { RouteFunction, RouteRequest };
export type { ActionDef, ActionFunction, ActionOptions, ActionResult, InlineChanges };
