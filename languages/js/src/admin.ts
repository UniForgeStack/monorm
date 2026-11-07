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
import { hostDispatch } from "./admin-host.js";
import * as grants from "./admin-permissions.js";
import type { RoleSeedReport, SyncReport } from "./admin-permissions.js";
import type { Database } from "./db.js";
import { ORMError } from "./errors.js";
import { Column, Ordering } from "./expr.js";
import type { ModelSpec } from "./meta.js";
import { registry as defaultRegistry, type ModelClass, type Registry } from "./model.js";
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

export interface ListConfig {
  readonly columns?: readonly AnyColumn[];
  readonly order?: readonly (AnyColumn | Ordering<any, any>)[];
  readonly pageSize?: number;
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
}

export interface ResourceConfig {
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

export interface AdminOptions {
  readonly permissions?: PermissionStrategy;
  /** The label of the permission names (`admin` by default). */
  readonly label?: string;
  /** Role name to pattern grants (`{ support: ["orders.*", "-shops.see_credential"] }`) that `seedRoles` writes to the auth tables. */
  readonly roles?: Readonly<Record<string, readonly string[]>>;
  /** The name, logo, colors, header links and environment label of the UI. */
  readonly brand?: BrandConfig;
  /** The order of the navigation groups; a resource names one of them with `group`. */
  readonly groups?: readonly string[];
}

/** One key field, the key values in key order, or an object of key fields. */
type KeyPart = string | number | bigint | boolean | Date;
export type RecordKey = KeyPart | readonly KeyPart[] | { readonly [field: string]: KeyPart };

export type { RoleSeedReport, SyncReport };

export interface AdminUser {
  readonly is_superuser?: boolean;
  readonly [key: string]: unknown;
}

const LIST_KEYS = new Set(["columns", "order", "pageSize"]);
const BRAND_KEYS = new Set(["name", "logo", "colors", "links", "environment"]);
const RESOURCE_KEYS = new Set(["list", "title", "group", "allow", "perms", "label", "fieldPermissions", "actions"]);

function label(model: AnyModel, ref: AnyColumn): string {
  if (ref.root.name !== model._meta.name) {
    throw new AdminConfigError(`${ref.text} belongs to ${ref.root.name}, not to ${model._meta.name}`);
  }
  return [...ref.path, ref.field.ir].join(".");
}

function listJson(model: AnyModel, config: ListConfig): Record<string, unknown> {
  const name = model._meta.name;
  for (const key of Object.keys(config)) {
    if (!LIST_KEYS.has(key)) throw new AdminConfigError(`${name}: unknown list option '${key}'`);
  }
  const out: Record<string, unknown> = {};
  if (config.columns !== undefined) out["columns"] = config.columns.map((c) => label(model, c));
  if (config.order !== undefined) {
    out["order"] = config.order.map((item) => {
      if (item instanceof Ordering) {
        if (!(item.expr instanceof Column)) throw new AdminConfigError(`${name}: order takes columns`);
        if (item.nulls !== undefined) throw new AdminConfigError(`${name}: order does not take a NULLs placement`);
        return (item.descending ? "-" : "") + label(model, item.expr);
      }
      return label(model, item);
    });
  }
  if (config.pageSize !== undefined) out["page_size"] = config.pageSize;
  return out;
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
  ) {
    if (login !== null && user !== null) throw new TypeError("testClient takes `user` (no login) or `secret` (login), not both");
    this.#user = user === null ? null : JSON.stringify(user);
    this.#login = login;
    this.ip = login?.ip ?? "127.0.0.1";
  }

  async request(method: string, path: string, options: RequestOptions = {}): Promise<AdminResponse> {
    const login = this.#login;
    if (login === null) {
      if (Object.keys(options).length > 0) throw new TypeError("the `user` client takes only a method and a path");
      const trace = new (native().Trace)();
      const { status, body } = await wait(() => this.site.request(this.database.engine, method, path, this.#user, trace));
      return new AdminResponse(status, body, trace.take().length);
    }
    this.#router ??= call(() =>
      this.site.client(this.database.engine, hostDispatch(this.database), login.secret, JSON.stringify(login.settings ?? {}), null),
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
  readonly #groups: string[];
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
    this.#groups = [...(options.groups ?? [])];
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
    const resources = [...this.#resources].map(([model, { model: cls, config }]) => ({
      model,
      ...(config.list === undefined ? {} : { list: listJson(cls, config.list) }),
      ...(config.title === undefined ? {} : { title: config.title }),
      ...(config.group === undefined ? {} : { group: config.group }),
      ...(config.allow === undefined ? {} : { allow: [...config.allow] }),
      ...(config.perms === undefined ? {} : { perms: [...config.perms] }),
      ...(config.label === undefined ? {} : { label: config.label }),
      ...(config.fieldPermissions === undefined ? {} : { field_permissions: [...config.fieldPermissions] }),
      ...(config.actions === undefined ? {} : { actions: [...config.actions] }),
    }));
    return JSON.stringify({
      prefix: this.prefix,
      permissions: this.#strategy,
      label: this.#label,
      resources,
      ...(this.#brand === undefined ? {} : { brand: this.#brand }),
      ...(this.#groups.length === 0 ? {} : { groups: this.#groups }),
    });
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

  /** Throws `AdminConfigError` when the config does not fit the schema. Needs no database. */
  check(): void {
    this.#site(this.registry);
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
    return new AdminTestClient(this.#site(database.registry), database, options.user ?? null, login, this.prefix);
  }

  /** @internal What `monorm admin` runs: serves this admin on the socket of `settingsJson`; `secret` is `MONORM_ADMIN_SECRET`. */
  serve(database: Database, settingsJson: string, secret: string | null = null): Promise<NativeAdminServer> {
    const site = this.#site(database.registry);
    let started: Promise<NativeAdminServer>;
    try {
      started = site.serve(database.engine, settingsJson, hostDispatch(database), secret);
    } catch (e) {
      throw new AdminSettingsError(e instanceof Error ? e.message : String(e));
    }
    return wait(() => started);
  }
}

const defaultAdmin = new Admin();

export const admin = {
  register: (model: AnyModel, config?: ResourceConfig): void => defaultAdmin.register(model, config),
  check: (): void => defaultAdmin.check(),
  url: (model: AnyModel, key: RecordKey): string => defaultAdmin.url(model, key),
  testClient: (database: Database, options?: Parameters<Admin["testClient"]>[1]): AdminTestClient => defaultAdmin.testClient(database, options),
};
