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
import type { Database } from "./db.js";
import { ORMError } from "./errors.js";
import { Column, Ordering } from "./expr.js";
import type { ModelSpec } from "./meta.js";
import { registry as defaultRegistry, type ModelClass, type Registry } from "./model.js";
import { call, native, wait, type NativeAdminServer, type NativeAdminSite } from "./native.js";

export const DEFAULT_PREFIX = "/admin/api";

/** The config names an unknown field, a list column through a to-many relation, or a bad value. */
export class AdminConfigError extends ORMError {
  override name = "AdminConfigError";
}

type AnyColumn = Column<any, any, any, any>;
type AnyModel = ModelClass<ModelSpec>;

export interface ListConfig {
  readonly columns?: readonly AnyColumn[];
  readonly order?: readonly (AnyColumn | Ordering<any, any>)[];
  readonly pageSize?: number;
}

export interface ResourceConfig {
  readonly list?: ListConfig;
}

export interface AdminUser {
  readonly is_superuser?: boolean;
  readonly [key: string]: unknown;
}

const LIST_KEYS = new Set(["columns", "order", "pageSize"]);

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
  ) {}

  json(): any {
    return JSON.parse(this.text);
  }
}

/**
 * Sends requests to the admin router in memory: no server, no port.
 *
 * `user` is a plain value that the client trusts. Its shape is a JSON object: `{ is_superuser: true }`
 * reads every resource, no user is a 401 and any other user is a 403.
 */
export class AdminTestClient {
  readonly #user: string | null;

  /** @internal */
  constructor(
    private readonly site: NativeAdminSite,
    private readonly database: Database,
    user: AdminUser | null,
  ) {
    this.#user = user === null ? null : JSON.stringify(user);
  }

  async request(method: string, path: string): Promise<AdminResponse> {
    const trace = new (native().Trace)();
    const { status, body } = await wait(() => this.site.request(this.database.engine, method, path, this.#user, trace));
    return new AdminResponse(status, body, trace.take().length);
  }

  get(path: string): Promise<AdminResponse> {
    return this.request("GET", path);
  }
}

/** The resources of one registry. */
export class Admin {
  prefix: string = DEFAULT_PREFIX;
  readonly #resources = new Map<string, { model: AnyModel; config: ResourceConfig }>();

  constructor(private readonly registry: Registry = defaultRegistry) {}

  /** Makes `model` a resource. Once any model is registered, only registered models are resources. */
  register(model: AnyModel, config: ResourceConfig = {}): void {
    const name = model._meta.name;
    if (this.#resources.has(name)) throw new AdminConfigError(`${name} is already registered`);
    for (const key of Object.keys(config)) {
      if (key !== "list") throw new AdminConfigError(`${name}: unknown option '${key}'`);
    }
    this.#resources.set(name, { model, config });
  }

  #configJson(): string {
    const resources = [...this.#resources].map(([model, { model: cls, config }]) => ({
      model,
      ...(config.list === undefined ? {} : { list: listJson(cls, config.list) }),
    }));
    return JSON.stringify({ prefix: this.prefix, resources });
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

  testClient(database: Database, options: { readonly user?: AdminUser | null } = {}): AdminTestClient {
    return new AdminTestClient(this.#site(database.registry), database, options.user ?? null);
  }

  /** @internal What `monorm admin` runs: serves this admin on the socket of `settingsJson`. */
  serve(database: Database, settingsJson: string): Promise<NativeAdminServer> {
    return wait(() => this.#site(database.registry).serve(database.engine, settingsJson));
  }
}

const defaultAdmin = new Admin();

export const admin = {
  register: (model: AnyModel, config?: ResourceConfig): void => defaultAdmin.register(model, config),
  check: (): void => defaultAdmin.check(),
  testClient: (database: Database, options?: { readonly user?: AdminUser | null }): AdminTestClient => defaultAdmin.testClient(database, options),
};
