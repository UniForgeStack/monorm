/**
 * Host-side writes of the admin: the default writer, the service wrappers, `Context` and `log`.
 *
 * The Rust service calls these functions with no transaction. A function owns its transactions
 * (`database.transaction(...)`) and answers an envelope that the service maps to a status:
 * `{ ok: true, key, audited }`, or `{ ok: false, kind: "not_found" | "conflict" | "invalid" }`.
 */
import type { HostFunction } from "./admin-host.js";
import type { Database } from "./db.js";
import { fieldAdapter, FieldAdapterError, type FieldAdapter } from "./adapters.js";
import { Decimal } from "./decimal.js";
import { Duration } from "./duration.js";
import { DoesNotExist, ValidationError, VersionConflict } from "./errors.js";
import { asCondition, Condition, Expression, IRContext, resolveNullRule, Slot, SlotParams } from "./expr.js";
import type { FieldMeta, ModelSpec } from "./meta.js";
import type { ModelClass, ModelMeta } from "./model.js";
import { parseTime } from "./date-micros.js";
import { parseDateTime } from "./nested.js";
import { allowWrites } from "./protection.js";
import { Prepared } from "./query.js";

type AnyModel = ModelClass<ModelSpec>;
type Loose = any;

/** The definition of one input control; see {@link input}. */
export class InputDef {
  constructor(
    readonly name: string,
    readonly type: string,
    readonly label: string | undefined,
    readonly required: boolean,
    readonly help: string | undefined,
    readonly choices: readonly string[],
    readonly secret: boolean = false,
  ) {}

  /** @internal */
  json(): Record<string, unknown> {
    return {
      name: this.name,
      type: this.type,
      required: this.required,
      choices: [...this.choices],
      ...(this.label === undefined ? {} : { label: this.label }),
      ...(this.help === undefined ? {} : { help: this.help }),
      ...(this.secret ? { secret: true } : {}),
    };
  }
}

export interface InputOptions {
  readonly type?: "text" | "textarea" | "number" | "checkbox" | "select" | "file" | "password";
  readonly label?: string;
  readonly required?: boolean;
  readonly help?: string;
  readonly choices?: readonly string[];
  /** The audit row stores a mask (`***`) in place of the value. */
  readonly secret?: boolean;
}

/**
 * A control of the form that is no field: `input("reason", { type: "textarea", required: true })`.
 * The value lives for one request and only the service reads it as `ctx.inputs[name]`.
 * A resource with an input needs a service.
 */
export function input(name: string, options: InputOptions = {}): InputDef {
  return new InputDef(name, options.type ?? "text", options.label, options.required ?? false, options.help, options.choices ?? [], options.secret ?? false);
}

export interface ContextOptions {
  readonly user?: Readonly<Record<string, unknown>> | null;
  readonly requestId?: string;
  readonly loaded?: Readonly<Record<string, unknown>> | null;
  readonly inputs?: Readonly<Record<string, unknown>> | null;
  /** The inputs as the audit row stores them (a secret input as a mask). The admin sets it; the default is `inputs`. */
  readonly auditInputs?: Readonly<Record<string, unknown>> | null;
  readonly database?: Database | null;
  readonly key?: Readonly<Record<string, unknown>> | null;
  readonly resource?: string;
  readonly op?: string;
  readonly ip?: string | null;
  readonly userAgent?: string | null;
  readonly inlines?: Readonly<Record<string, InlineChanges>> | null;
  /** An action: the number of rows in the query set (at most 10,000), whether the count hit that cap, and how the rows were chosen (`keys`, `all` or `filter`). */
  readonly count?: number;
  readonly capped?: boolean;
  readonly selection?: string;
}

/**
 * The checked change set of one inline: `create` is a list of value objects, `update` a list of `{ key, set, loaded, before }`,
 * `delete` a list of `{ key, before }`. `pin` pairs each child field with the parent field it holds. The values are by field name.
 */
export interface InlineChanges {
  readonly model: AnyModel;
  readonly pin: readonly { readonly child: string; readonly parent: string; readonly value: unknown }[];
  readonly create: readonly Record<string, unknown>[];
  readonly update: readonly { readonly key: Record<string, unknown>; readonly set: Record<string, unknown>; readonly loaded: Record<string, unknown>; readonly before: Record<string, unknown> }[];
  readonly delete: readonly { readonly key: Record<string, unknown>; readonly before: Record<string, unknown> }[];
}

export interface AuditEntry {
  readonly action: string;
  readonly resource: string;
  readonly key: string[] | null;
  readonly changes: Record<string, unknown>;
}

/**
 * What a service function knows about the request.
 *
 * `user` is the plain user of the admin, `requestId` the id of the request, `loaded` the values that the form showed
 * (by field name; empty on a create) and `inputs` the values of the `input` controls. `database` is the database
 * of the request. A unit test builds a `Context` by hand and calls the service function: no admin is needed.
 */
export class Context {
  readonly user: Record<string, unknown>;
  readonly requestId: string;
  readonly loaded: Record<string, unknown>;
  readonly inputs: Record<string, unknown>;
  readonly database: Database | null;
  readonly key: Record<string, unknown> | null;
  readonly resource: string;
  readonly op: string;
  readonly ip: string | null;
  readonly userAgent: string | null;
  /** The checked rows of the inlines that the form sent with the record. The service writes them in its own transaction: deletes, then updates, then creates. A service that ignores them drops the rows. */
  readonly inlines: Readonly<Record<string, InlineChanges>>;
  /** An action: the number of rows in its query set, at most 10,000. */
  readonly count: number;
  /** An action: true when the rows were more than the cap of 10,000. */
  readonly capped: boolean;
  /** An action: `keys` (a selection), `all` (every row that matches) or `filter` (a list action). */
  readonly selection: string;
  readonly audits: AuditEntry[] = [];
  /** @internal */
  readonly auditKeys: { id: number; key: string[] | null }[] = [];
  /** @internal */
  readonly auditInputs: Record<string, unknown>;
  /** @internal */
  audited = false;
  /** @internal */
  byAdmin = false;
  /** @internal */
  meta: ModelMeta | null = null;

  constructor(options: ContextOptions = {}) {
    this.user = Object.fromEntries(Object.entries(options.user ?? {}).filter(([k]) => k !== "password_hash"));
    this.requestId = options.requestId ?? "";
    this.loaded = { ...(options.loaded ?? {}) };
    this.inputs = { ...(options.inputs ?? {}) };
    this.auditInputs = { ...(options.auditInputs ?? this.inputs) };
    this.database = options.database ?? null;
    this.key = options.key == null ? null : { ...options.key };
    this.resource = options.resource ?? "";
    this.op = options.op ?? "";
    this.ip = options.ip ?? null;
    this.userAgent = options.userAgent ?? null;
    this.inlines = { ...(options.inlines ?? {}) };
    this.count = options.count ?? 0;
    this.capped = options.capped ?? false;
    this.selection = options.selection ?? "keys";
  }

  /**
   * Writes the audit row of this write on the database of the request (in the transaction that is open now).
   *
   * The row holds the user, the action (the operation by default), the key of the record, the inputs (a secret input as a mask) and `changes`.
   * A create passes the new record (an instance, a key value or an object of the key) so that the row names it.
   * With this call the admin writes no audit row of its own for the request.
   */
  async audit(record?: unknown, options: { readonly action?: string; readonly changes?: Readonly<Record<string, unknown>> } = {}): Promise<void> {
    let key: Record<string, unknown> | null = this.key;
    const meta = metaOf(record) ?? this.meta;
    if (record !== undefined && record !== null && meta !== null) key = keyOf(meta, record);
    if (key === null && this.op === "create") throw new Error("ctx.audit(record) on a create needs the new record: the row names it");
    const extra: Record<string, unknown> = { request_id: this.requestId, inputs: { ...this.auditInputs }, service: true };
    if (this.ip !== null) extra["ip"] = this.ip;
    if (this.userAgent !== null) extra["user_agent"] = this.userAgent;
    Object.assign(extra, options.changes ?? {});
    const action = options.action ?? (this.op || "service");
    const resource = meta !== null && meta.name !== this.meta?.name ? meta.name : this.resource || meta?.name || "";
    const scalars = key === null || Object.keys(key).length === 0 ? null : Object.values(meta === null ? key : keyScalars(meta, key)).map((v) => String(v));
    this.audits.push({ action, resource, key: scalars, changes: extra });
    this.audited = true;
    if (this.database !== null) {
      // Under the admin the row keeps a blank key text: Rust writes it from `auditKeys`, the only place that encodes a key.
      // A context built by hand (a unit test) has no Rust: its row holds the plain key values joined with `,`.
      const text = this.byAdmin || scalars === null ? "" : scalars.join(",");
      const row = await writeLog(this.database, userKey(this.user), action, resource, text, "", extra);
      this.auditKeys.push({ id: Number(row.id), key: scalars });
    }
  }
}

function metaOf(record: unknown): ModelMeta | null {
  const ctor = typeof record === "object" && record !== null ? (record as { constructor?: { meta?: ModelMeta } }).constructor : undefined;
  return ctor?.meta ?? null;
}

function userKey(user: Readonly<Record<string, unknown>>): string {
  const key = user["key"];
  return key === undefined || key === null ? "" : String(key);
}

/** One key value as the admin writes it in JSON and in the key text. */
function scalarJson(field: FieldMeta, value: unknown): unknown {
  if (value instanceof Date) return field.type === "date" ? value.toISOString().slice(0, 10) : value.toISOString();
  if (typeof value === "bigint" || value instanceof Decimal || value instanceof Duration) return value.toString();
  if (value instanceof Uint8Array) return Buffer.from(value).toString("base64");
  return value;
}

function keyScalars(meta: ModelMeta, key: Readonly<Record<string, unknown>>): Record<string, unknown> {
  return Object.fromEntries(Object.entries(key).map(([name, value]) => {
    const field = meta.fieldByIr.get(name) ?? meta.fields.get(name);
    return [name, field === undefined ? value : scalarJson(field, value)];
  }));
}

/** The key of `record` by the schema names of the key fields: an instance, an object of the key, a value or a list of values. */
function keyOf(meta: ModelMeta, record: unknown): Record<string, unknown> {
  const fields = meta.key;
  const read = (get: (f: FieldMeta) => unknown) => Object.fromEntries(fields.map((f) => [f.ir, get(f)]));
  if (Array.isArray(record)) {
    if (record.length !== fields.length) throw new TypeError(`${meta.name}: cannot read a key from ${String(record)}`);
    return read((f) => record[fields.indexOf(f)]);
  }
  if (typeof record === "object" && record !== null && !(record instanceof Date) && !(record instanceof Decimal) && !(record instanceof Duration)) {
    const r = record as Record<string, unknown>;
    if (fields.every((f) => f.name in r)) return read((f) => r[f.name]);
    if (fields.every((f) => f.ir in r)) return read((f) => r[f.ir]);
    throw new TypeError(`${meta.name}: cannot read a key from ${JSON.stringify(record)}`);
  }
  if (fields.length === 1) return read(() => record);
  throw new TypeError(`${meta.name}: cannot read a key from ${String(record)}`);
}

/**
 * Writes one row of the audit log from host code outside the admin: a job, a script, a webhook.
 *
 * `user` is the user of the row (a user object: its `key`, else its `id`; a user key; or `null`), `recordKey` the key of the record
 * (text, or a key value). The row goes through the database `using` and joins the transaction that is open.
 * It needs the `AdminLogEntry` model of the admin app in the registry.
 */
export async function log(
  action: string,
  options: { readonly user?: unknown; readonly resource?: string; readonly recordKey?: unknown; readonly label?: string; readonly changes?: Readonly<Record<string, unknown>>; readonly using: Database },
): Promise<void> {
  const { user, resource = "", recordKey = "", label = "", changes, using } = options;
  const key = typeof user === "object" && user !== null ? ((user as Record<string, unknown>)["key"] ?? (user as Record<string, unknown>)["id"]) : user;
  const text = typeof recordKey === "string" ? recordKey : String(scalarKey(recordKey));
  await writeLog(using, key === undefined || key === null ? "" : String(key), action, resource, text, label, changes === undefined ? null : { ...changes });
}

function scalarKey(value: unknown): unknown {
  if (value instanceof Date) return value.toISOString();
  return value;
}

async function writeLog(db: Database, user: string, action: string, resource: string, recordKey: string, label: string, changes: Readonly<Record<string, unknown>> | null): Promise<Loose> {
  let entry: Loose;
  try {
    entry = db.registry.get("AdminLogEntry").model;
  } catch {
    throw new Error("the admin log needs the AdminLogEntry model: import the admin app schema");
  }
  return entry.objects.using(db).insert({ userKey: user, action, resource, recordKey, recordLabel: label, changes });
}

// -- values from the JSON of the request ---------------------------------------------------------------------------

function decodeOne(field: FieldMeta, value: unknown): unknown {
  if (value === null || value === undefined) return value;
  if (typeof value === "string") {
    switch (field.type) {
      case "date_time": return parseDateTime(value) ?? new Date(value);
      case "time": return parseTime(value) ?? value;
      case "date": {
        const m = /^(\d{4})-(\d\d)-(\d\d)$/.exec(value);
        return m === null ? value : new Date(Date.UTC(+m[1]!, +m[2]! - 1, +m[3]!));
      }
      case "big_int": return /^-?\d+$/.test(value) ? BigInt(value) : value;
      case "decimal": return new Decimal(value);
      case "duration": return Duration.parse(value);
      case "bytes": return new Uint8Array(Buffer.from(value, "base64"));
    }
  }
  return value;
}

/** The JSON of a view field that its adapter refuses: the form shows it under the field. */
export class ViewError extends Error {
  constructor(readonly field: string, message: string) {
    super(message);
  }
}

/** The adapter of a field that the admin shows as JSON, or `undefined`. */
function viewAdapter(meta: ModelMeta, name: string): FieldAdapter | undefined {
  const adapter = fieldAdapter(meta, name);
  return adapter?.adminMode?.() === "json" ? adapter : undefined;
}

function shownAsText(meta: ModelMeta, name: string): boolean {
  return fieldAdapter(meta, name)?.adminMode?.() === "text";
}

/** What the admin shows for stored bytes: the JSON, or "N bytes" for bytes that do not decode or an adapter with no JSON. */
export function viewOf(adapter: FieldAdapter, stored: Uint8Array | null | undefined): unknown {
  if (stored === null || stored === undefined) return null;
  if (adapter.adminMode?.() === "json" && adapter.adminToView !== undefined) {
    try {
      return adapter.adminToView(stored);
    } catch (e) {
      // The bytes do not decode: show the size. Any other error is a bug of the adapter.
      if (!(e instanceof FieldAdapterError)) throw e;
    }
  }
  return `${stored.byteLength} bytes`;
}

/** The compare of the Rust core (`json::same_json`): numbers compare by value, so `1` and `1.0` match. */
export function sameJson(a: unknown, b: unknown): boolean {
  if (Array.isArray(a) || Array.isArray(b)) return Array.isArray(a) && Array.isArray(b) && a.length === b.length && a.every((x, i) => sameJson(x, b[i]));
  if (typeof a === "object" && a !== null && typeof b === "object" && b !== null) {
    const ka = Object.keys(a);
    return ka.length === Object.keys(b).length && ka.every((k) => k in b && sameJson((a as Loose)[k], (b as Loose)[k]));
  }
  return a === b;
}

/**
 * The values of a request (JSON, by schema name) as the values of the model: by field name, with dates, big integers and decimals.
 * A view field takes the value of its JSON. A JSON that the adapter refuses throws {@link ViewError} with `strict`, else stays JSON.
 */
export function decodeValues(meta: ModelMeta, values: Readonly<Record<string, unknown>>, strict = false): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [name, value] of Object.entries(values)) {
    const field = meta.fieldByIr.get(name);
    if (field === undefined) {
      out[name] = value;
      continue;
    }
    const adapter = value === null || value === undefined ? undefined : viewAdapter(meta, field.name);
    if (shownAsText(meta, field.name)) out[field.name] = value;
    else if (adapter?.adminFromView !== undefined) {
      try {
        out[field.name] = adapter.adminFromView(value);
      } catch (e) {
        if (!(e instanceof FieldAdapterError)) throw e;
        if (strict) throw new ViewError(name, e.message);
        out[field.name] = value;
      }
    } else out[field.name] = field.array && Array.isArray(value) ? value.map((v) => decodeOne(field, v)) : decodeOne(field, value);
  }
  return out;
}

// -- the filters of the resource -----------------------------------------------------------------------------------

/** The filter of a prepared `queryset`, with the user key put into its `param("user")`. */
class Bound extends Condition<any, any> {
  constructor(
    private readonly conditions: readonly Condition<any, any>[],
    private readonly user: unknown,
  ) {
    super();
  }

  ir(ctx: IRContext): Loose {
    const inner = new SlotParams();
    const irs = this.conditions.map((c) => (resolveNullRule(c) as Expression<unknown>).ir(new IRContext(ctx.root, inner)));
    return this.remap(irs.length === 1 ? irs[0] : { t: "and", items: irs }, inner, ctx.params);
  }

  private remap(ir: Loose, inner: readonly unknown[], params: unknown[]): Loose {
    if (Array.isArray(ir)) return ir.map((i) => this.remap(i, inner, params));
    if (ir === null || typeof ir !== "object") return ir;
    if (ir["t"] === "param") {
      let value = inner[ir["i"]];
      if (value instanceof Slot) {
        if (value.name !== "user") throw new Error(`the queryset takes the param 'user', not '${value.name}'`);
        value = this.user;
      }
      params.push(value);
      return { ...ir, i: params.length - 1 };
    }
    return Object.fromEntries(Object.entries(ir).map(([k, v]) => [k, this.remap(v, inner, params)]));
  }
}

/** The conditions the admin applies to one write: the `queryset` of the user and the `can` rule of the operation. */
async function resourceFilters(config: Loose, user: Loose, op: string): Promise<Condition<any, any>[]> {
  const out: Condition<any, any>[] = [];
  const scope = config.queryset;
  if (scope instanceof Prepared) out.push(new Bound((scope as Loose)["qs"].state.filters, user?.key));
  else if (typeof scope === "function") {
    const found = await scope(user);
    for (const c of Array.isArray(found) ? found : [found]) if (c !== undefined && c !== null) out.push(toCondition(c));
  }
  const rule = config.can?.[op === "update" ? "change" : op === "delete" ? "delete" : ""];
  if (rule !== undefined && rule !== null) out.push(toCondition(rule));
  return out;
}

function toCondition(node: unknown): Condition<any, any> {
  return (node instanceof Condition ? node : node instanceof Expression ? node : asCondition(node)) as Condition<any, any>;
}

function oneRow(model: Loose, db: Database, key: Readonly<Record<string, unknown>>, filters: readonly Condition<any, any>[]): Loose {
  const meta: ModelMeta = model._meta;
  const conditions = Object.entries(key).map(([name, value]) => {
    const field = meta.fieldByIr.get(name);
    if (field === undefined) throw new Error(`${meta.name} has no field ${name}`);
    return model[field.name].eq(decodeOne(field, value));
  });
  return model.objects.using(db).filter(...conditions, ...filters);
}

// -- the envelope --------------------------------------------------------------------------------------------------

function keyJson(meta: ModelMeta, record: unknown): Record<string, unknown> {
  return keyScalars(meta, keyOf(meta, record));
}

function irPath(meta: ModelMeta, path: string): string {
  const [head, ...rest] = path.split(".");
  const field = meta.fields.get(head!);
  return field === undefined ? path : [field.ir, ...rest].join(".");
}

function invalid(meta: ModelMeta, e: ValidationError): Record<string, unknown> {
  const fields: Record<string, readonly string[]> = {};
  for (const [path, texts] of e.fields) fields[irPath(meta, path)] = [...texts];
  return { ok: false, kind: "invalid", fields, form: [...e.nonFieldMessages] };
}

/** The envelope of an error that a write answers by status, or `undefined` for any other error. */
function viewInvalid(e: ViewError): Record<string, unknown> {
  return { ok: false, kind: "invalid", fields: { [e.field]: [e.message] }, form: [] };
}

function failure(meta: ModelMeta, e: unknown): Record<string, unknown> | undefined {
  if (e instanceof ValidationError) return invalid(meta, e);
  if (e instanceof ViewError) return viewInvalid(e);
  // A field adapter refuses a value the form cannot build (for example base64 bytes for a message field).
  if (e instanceof FieldAdapterError) return { ok: false, kind: "invalid", fields: {}, form: [e.message] };
  if (e instanceof VersionConflict) return { ok: false, kind: "conflict" };
  if (e instanceof DoesNotExist) return { ok: false, kind: "not_found" };
  return undefined;
}

/** The change set of the inlines with the values of each row decoded to the types of the child model. */
function decodeInlines(raw: Loose | null | undefined, models: ReadonlyMap<string, AnyModel>): Record<string, InlineChanges> {
  const out: Record<string, InlineChanges> = {};
  for (const [name, spec] of Object.entries(raw ?? {}) as [string, Loose][]) {
    const child = models.get(spec.model);
    if (child === undefined) throw new Error(`no admin model ${String(spec.model)}`);
    const meta = child._meta;
    out[name] = {
      model: child,
      pin: spec.pin,
      create: spec.create.map((r: Loose) => decodeValues(meta, r.set)),
      update: spec.update.map((r: Loose) => ({ key: r.key, set: decodeValues(meta, r.set), loaded: decodeValues(meta, r.loaded), before: decodeValues(meta, r.before) })),
      delete: spec.delete.map((r: Loose) => ({ key: r.key, before: decodeValues(meta, r.before) })),
    };
  }
  return out;
}

function contextOf(database: Database, args: Loose, meta: ModelMeta, models: ReadonlyMap<string, AnyModel>): Context {
  const ctx = new Context({
    user: args.user,
    requestId: args.request_id ?? "",
    loaded: decodeValues(meta, args.loaded ?? {}),
    inputs: args.inputs ?? {},
    auditInputs: args.audit_inputs ?? null,
    database,
    key: args.key,
    resource: args.resource ?? meta.name,
    op: args.op ?? "",
    ip: args.ip,
    userAgent: args.user_agent,
    inlines: decodeInlines(args.inlines, models),
    count: args.count ?? 0,
    capped: args.capped === true,
    selection: args.selection ?? "keys",
  });
  ctx.meta = meta;
  ctx.byAdmin = true;
  return ctx;
}

function guarded<T>(model: AnyModel, args: Loose, run: () => Promise<T>): Promise<T> {
  return args.allow_protected ? allowWrites([model], run) : run();
}

/** The compare of the Rust core (`json::same`): a decimal compares by number, so `5` and `5.00` match. */
function same(type: string, stored: unknown, loaded: unknown): boolean {
  if (type === "decimal" && stored instanceof Decimal && loaded instanceof Decimal) return stored.equals(loaded);
  if (stored instanceof Date && loaded instanceof Date) return stored.getTime() === loaded.getTime();
  if (stored instanceof Duration && loaded instanceof Duration) return stored.equals(loaded);
  if (stored === loaded) return true;
  if (stored === null || stored === undefined || loaded === null || loaded === undefined) return false;
  if (stored instanceof Uint8Array && loaded instanceof Uint8Array) return Buffer.from(stored).equals(Buffer.from(loaded));
  if (typeof stored === "object" || typeof loaded === "object") return JSON.stringify(stored) === JSON.stringify(loaded);
  return String(stored) === String(loaded);
}

/** The stored bytes of a loaded value, or `undefined` when the field stores something else. */
function storedBytes(value: unknown): Uint8Array | undefined {
  return value instanceof Uint8Array ? value : undefined;
}

/**
 * Reads the row under a lock and compares the loaded values of the `compare` fields with it. `null` when the row is gone, else the stored values of the compared fields and of `extra`.
 * The compare runs in the write transaction, so a concurrent edit between the load and the save is a `VersionConflict`.
 * It selects the columns, so a field adapter does not decode them: a row with a value that does not decode still compares.
 */
async function checkLoaded(meta: ModelMeta, qs: Loose, args: Loose, extra: readonly string[]): Promise<Record<string, unknown> | null> {
  const loadedJson: Record<string, unknown> = args.loaded ?? {};
  const names: string[] = (args.compare ?? []).filter((n: string) => n in loadedJson);
  const fieldOf = (n: string): FieldMeta | undefined => meta.fieldByIr.get(n);
  const columns = new Map<string, FieldMeta>();
  for (const f of meta.key) columns.set(f.name, f);
  for (const n of [...names, ...extra]) {
    const field = fieldOf(n);
    if (field !== undefined) columns.set(field.name, field);
  }
  const items = Object.fromEntries([...columns].map(([name, field]) => [name, meta.column(field)]));
  const row = await (args.lock ? qs.lock() : qs).select(items).first();
  if (row === null || row === undefined) return null;
  const viewOfName = (n: string): FieldAdapter | undefined => {
    const field = fieldOf(n);
    return field === undefined ? undefined : viewAdapter(meta, field.name);
  };
  const loaded = decodeValues(meta, Object.fromEntries(names.filter((n) => viewOfName(n) === undefined).map((n) => [n, loadedJson[n]])));
  for (const n of names) {
    const field = fieldOf(n);
    const name = field?.name ?? n;
    const view = viewOfName(n);
    if (view !== undefined) {
      // The form loaded the JSON view of the stored bytes: compare the view of the stored bytes now.
      if (!sameJson(viewOf(view, storedBytes(row[name])), loadedJson[n])) throw new VersionConflict(`${meta.name} ${JSON.stringify(args.key)} was changed since the form loaded`);
      continue;
    }
    let current = row[name];
    let wanted = loaded[name];
    // An adapted field shows a decoded value; the form loaded its stored bytes (base64), so compare those.
    if (fieldAdapter(meta, name) !== undefined && storedBytes(current) !== undefined && typeof wanted === "string") wanted = Buffer.from(wanted, "base64");
    if (!same(field?.type ?? "", current, wanted)) throw new VersionConflict(`${meta.name} ${JSON.stringify(args.key)} was changed since the form loaded`);
  }
  return row;
}

/**
 * Refuses a JSON edit that would drop data: a message field that the JSON view cannot show (an unknown field) would not survive the save.
 * A stored value that does not decode has nothing to lose, so the repair goes through.
 */
function checkLossless(meta: ModelMeta, row: Record<string, unknown>, values: Record<string, unknown>): void {
  for (const name of Object.keys(values)) {
    const view = viewAdapter(meta, name);
    const stored = storedBytes(row[name]);
    if (view?.adminToView === undefined || view.adminFromView === undefined || stored === undefined) continue;
    let kept: unknown;
    let whole: unknown;
    try {
      kept = view.validate(view.adminFromView(view.adminToView(stored)));
      whole = view.validate(view.decode(stored));
    } catch (e) {
      if (e instanceof FieldAdapterError) continue;
      throw e;
    }
    if (kept instanceof Uint8Array && whole instanceof Uint8Array && !Buffer.from(kept).equals(Buffer.from(whole))) {
      throw new ViewError(name, "the stored value has fields this version cannot show; edit it with a newer version");
    }
  }
}

/** The audit row of a default write, inside the write transaction. Answers its id. Rust sends the key text of an update and a delete; the admin sets the label, the key text of a create and the diff after it reads the record, by the id that this function answers. */
async function writeAudit(database: Database, meta: ModelMeta, args: Loose): Promise<unknown> {
  const changes: Record<string, unknown> = { request_id: args.request_id ?? "", inputs: { ...(args.audit_inputs ?? {}) }, service: false };
  if (args.ip !== undefined && args.ip !== null) changes["ip"] = args.ip;
  if (args.user_agent !== undefined && args.user_agent !== null) changes["user_agent"] = args.user_agent;
  const entry = await writeLog(database, userKey(args.user ?? {}), args.op, args.resource || meta.name, args.key_text ?? "", "", changes);
  return entry.id;
}

/** A row of an `update_rows` call answered a failure: it ends the loop and rolls the transaction back. */
class RowFailed extends Error {
  constructor(
    readonly index: number,
    readonly envelope: Loose,
  ) {
    super("a row of the list save failed");
  }
}

/**
 * Lets a host write function answer an `update_rows` call: one transaction, `fn` once per row in the order of `rows` (key order).
 * The first failure ends the loop and rolls back every row; the answer names the row by its `index`.
 * A failure envelope that returns inside `transaction()` would commit, so the loop throws to roll back.
 */
function perRow(fn: HostFunction): HostFunction {
  return async (database, args) => {
    if (args.op !== "update_rows") return await fn(database, args);
    try {
      const results = await database.transaction(async () => {
        const out: unknown[] = [];
        for (const [index, row] of (args.rows as Loose[]).entries()) {
          const answer = (await fn(database, row)) as Loose;
          if (!answer.ok) throw new RowFailed(index, answer);
          out.push(answer);
        }
        return out;
      });
      return { ok: true, results };
    } catch (e) {
      if (e instanceof RowFailed) return { ...e.envelope, index: e.index };
      throw e;
    }
  };
}

/**
 * The host function `admin.write`'s share for one model: a query-set write with the filters of the resource.
 *
 * It runs in one transaction: the compare of the loaded values, the write and the audit row commit or roll back together.
 */
export function defaultWriter(model: AnyModel, config: Loose): HostFunction {
  const meta = model._meta;
  return perRow(async (database, args) => {
    const bulk = Array.isArray(args.filters);
    const op: string = args.op;
    try {
      // A bulk delete sends a query set: `values` are the binds of its filters, not field values.
      const values = bulk ? {} : decodeValues(meta, args.values ?? {}, true);
      return await guarded(model, args, () =>
        database.transaction(async () => {
          if (bulk) {
            // The admin writes the summary row of a bulk delete.
            const n = Number(await querySet(model, database, args).delete());
            return { ok: true, audited: false, affected: n, result: { kind: "message", message: `Deleted ${n} row(s).` } };
          }
          if (op === "create") {
            const created = await (model as Loose).objects.using(database).insert(values);
            const key = keyJson(meta, created);
            return { ok: true, key, audited: true, audit_id: await writeAudit(database, meta, args) };
          }
          const qs = oneRow(model, database, args.key, await resourceFilters(config, args.user, op));
          const row = await checkLoaded(meta, qs, args, op === "update" ? Object.keys(values) : []);
          if (row === null) return { ok: false, kind: "not_found" };
          if (op === "update") checkLossless(meta, row, values);
          const done = op === "update" ? (Object.keys(values).length > 0 ? await qs.update(values) : await qs.exists()) : await qs.delete();
          if (!done) return { ok: false, kind: "not_found" };
          return { ok: true, key: args.key, audited: true, audit_id: await writeAudit(database, meta, args) };
        }),
      );
    } catch (e) {
      const envelope = failure(meta, e);
      if (envelope === undefined) throw e;
      return envelope;
    }
  });
}

/** The host function of one service function (`service.<Resource>.<op>`). */
export function serviceFunction(model: AnyModel, config: Loose, op: string, fn: (...args: any[]) => unknown, models: ReadonlyMap<string, AnyModel> = new Map()): HostFunction {
  const meta = model._meta;
  return perRow(async (database, args) => {
    const ctx = contextOf(database, args, meta, models);
    const bulk = Array.isArray(args.filters);
    let result: unknown;
    try {
      // A bulk delete sends the filters of its query set and a list of binds in `values`.
      const values = bulk ? {} : decodeValues(meta, args.values ?? {}, true);
      result = await guarded(model, args, async () => {
        if (op === "create") return await fn(ctx, values);
        const qs = bulk ? querySet(model, database, args) : oneRow(model, database, args.key, await resourceFilters(config, args.user, op));
        return await (op === "update" ? fn(ctx, qs, values) : fn(ctx, qs));
      });
    } catch (e) {
      const envelope = failure(meta, e);
      if (envelope === undefined) throw e;
      return envelope;
    }
    const out: Record<string, unknown> = { ok: true, audited: ctx.audited, audit_keys: ctx.auditKeys };
    if (op === "create") {
      if (result === undefined || result === null) return { ok: false, kind: "error", message: `the create service of ${meta.name} returns the new record` };
      out["key"] = keyJson(meta, result);
    }
    return out;
  });
}

/**
 * The host function `admin.write`: one function for every resource, chosen by the model name of the call.
 * `models` are the child models of the inlines: one with no page of its own gets a writer with no filters.
 */
export function writerHost(resources: ReadonlyMap<string, { readonly model: AnyModel; readonly config: Loose }>, models: ReadonlyMap<string, AnyModel> = new Map()): HostFunction {
  const writers = new Map<string, HostFunction>([...models].map(([name, model]) => [name, defaultWriter(model, {})] as const));
  for (const [name, { model, config }] of resources) writers.set(name, defaultWriter(model, config));
  return async (database, args) => {
    const writer = writers.get(args.model);
    if (writer === undefined) throw new Error(`no admin resource ${String(args.model)}`);
    return await writer(database, args);
  };
}

/** The host function `admin.view`: the view of the stored bytes (base64 or null) of one field, for a page or a record. */
export function viewHost(models: ReadonlyMap<string, AnyModel>): HostFunction {
  return async (_database, args) => {
    const model = models.get(args.model);
    if (model === undefined) throw new Error(`no admin model ${String(args.model)}`);
    const meta = model._meta;
    const adapter = fieldAdapter(meta, meta.fieldByIr.get(args.field)?.name ?? args.field);
    if (adapter === undefined) throw new Error(`${meta.name}.${String(args.field)} has no field adapter`);
    return (args.values as (string | null)[]).map((v) => viewOf(adapter, v === null ? null : new Uint8Array(Buffer.from(v, "base64"))));
  };
}

// -- actions -------------------------------------------------------------------------------------------------------

export const PLACEMENTS = ["bulk", "row", "detail", "list", "submit"] as const;
export type Placement = (typeof PLACEMENTS)[number];
const LEVELS = ["info", "success", "warning", "error"] as const;
export type MessageLevel = (typeof LEVELS)[number];

/** The result of an action that shows a message. */
export function message(text: string, level: MessageLevel = "info"): ActionResult {
  if (!LEVELS.includes(level)) throw new RangeError(`the level of a message is one of ${LEVELS.join(", ")}`);
  return { kind: "message", message: text, level };
}

/** The result of an action that sends the browser to a path of the admin site (it starts with `/`). */
export function redirect(to: string): ActionResult {
  return { kind: "redirect", to };
}

/** The result of an action that sends a file to the browser. */
export function download(filename: string, content: Uint8Array | string, contentType = "application/octet-stream"): ActionResult {
  const data = typeof content === "string" ? Buffer.from(content, "utf8") : Buffer.from(content);
  return { kind: "download", filename, content_type: contentType, base64: data.toString("base64") };
}

/** The result of an action that only reloads the page. A function that returns nothing has this result. */
export function refresh(): ActionResult {
  return { kind: "refresh" };
}

export type ActionResult = { readonly kind: "message" | "redirect" | "download" | "refresh"; readonly [key: string]: unknown };

export interface ActionOptions {
  /** The name of the action in the URL and the audit row; the name of the function by default. */
  readonly name?: string;
  readonly label?: string;
  /** `bulk` (default): a selection or all rows that match. `row`, `detail`, `list` (all rows that match the filters) and `submit` (next to the save button). */
  readonly placement?: Placement;
  /** The name part of `<label>.<permission>_<resource>`. An action with no `permission` needs the manage permission of the resource. */
  readonly permission?: string;
  readonly confirm?: string;
  readonly inputs?: readonly InputDef[];
  /** A condition: the action is off on the rows where it is false, and the query set leaves them out. */
  readonly appliesTo?: unknown;
  /** A row permission condition that may read `param("user")`. */
  readonly can?: unknown;
  /** Use the action's permission condition in place of the resource change rule. */
  readonly replaceChange?: boolean;
}

export type ActionFunction = (ctx: Context, qs: Loose) => Promise<ActionResult | string | void | undefined | null> | ActionResult | string | void | undefined | null;

const ACTION = Symbol.for("monorm.admin.action");

/** The definition that {@link action} put on a function. */
export interface ActionDef extends ActionOptions {
  readonly name: string;
  readonly fn: ActionFunction;
}

/**
 * Marks a function as an admin action: `action(async (ctx, qs) => {...}, { name: "refund", placement: "bulk", confirm: "Refund these?" })`.
 *
 * `qs` is the query set of the rows (with the `queryset` filter, the `can` rule and `appliesTo`), `ctx.inputs` holds the values of
 * `inputs` and `ctx.count` the number of rows. Loop with `for await (const batch of qs.batches(500))`. The function returns nothing (a refresh),
 * a string (a message) or `message`, `redirect`, `download` or `refresh`. It returns the same function: a unit test calls it with a `Context`
 * and a query set and needs no admin.
 */
export function action<F extends ActionFunction>(fn: F, options: ActionOptions = {}): F {
  const def: ActionDef = { ...options, name: options.name ?? fn.name, fn };
  Object.defineProperty(fn, ACTION, { value: def, configurable: true });
  return fn;
}

/** The definition of a function that {@link action} marked, or `undefined`. */
export function actionDef(item: unknown): ActionDef | undefined {
  return typeof item === "function" ? ((item as Loose)[ACTION] as ActionDef | undefined) : undefined;
}

/** A bind of the engine (`{ $: tag, v }` for the types that JSON cannot tell apart) as a JavaScript value. */
function untag(value: unknown): unknown {
  if (value === null || typeof value !== "object" || Array.isArray(value) || !("$" in value)) return value;
  const { $: tag, v: raw } = value as { $: string; v: any };
  switch (tag) {
    case "bigint": return BigInt(raw);
    case "duration": return Duration.fromMicros(BigInt(raw));
    case "datetime": return parseDateTime(raw) ?? new Date(raw);
    case "date": {
      const m = /^(\d{4})-(\d\d)-(\d\d)$/.exec(raw);
      return m === null ? raw : new Date(Date.UTC(+m[1]!, +m[2]! - 1, +m[3]!));
    }
    case "time": return parseTime(raw) ?? raw;
    case "decimal": return new Decimal(raw);
    case "bytes": return new Uint8Array(Buffer.from(raw, "base64"));
    case "array": return (raw as unknown[]).map(untag);
    default: return raw;
  }
}

/** A filter that Rust built (engine IR) with the values of its parameters. */
class IrFilter extends Condition<any, any> {
  constructor(
    private readonly node: Loose,
    private readonly values: readonly unknown[],
  ) {
    super();
  }

  ir(ctx: IRContext): Loose {
    return this.remap(this.node, ctx.params);
  }

  private remap(ir: Loose, params: unknown[]): Loose {
    if (Array.isArray(ir)) return ir.map((i) => this.remap(i, params));
    if (ir === null || typeof ir !== "object") return ir;
    if (ir["t"] === "param" || ir["t"] === "in_param") {
      params.push(this.values[ir["i"]]);
      return { ...ir, i: params.length - 1 };
    }
    return Object.fromEntries(Object.entries(ir).map(([k, v]) => [k, this.remap(v, params)]));
  }
}

/** The query set of an action call: the filters of the payload, on the database of the request. */
export function querySet(model: Loose, database: Database, args: Loose): Loose {
  const values = ((args.values ?? []) as unknown[]).map(untag);
  let qs = model.objects.using(database);
  const filters = ((args.filters ?? []) as unknown[]).map((f) => new IrFilter(f, values));
  // An action on every row, with no filter, is a write the staff asked for: `.all()` lets `qs.update(...)` run.
  qs = filters.length > 0 ? qs.filter(...filters) : qs.all();
  if (Array.isArray(args.without) && args.without.length > 0) qs = qs.withoutDefaults(...args.without);
  return qs;
}

function resultJson(result: unknown): Record<string, unknown> {
  if (result === undefined || result === null) return refresh();
  if (typeof result === "string") return message(result);
  if (typeof result === "object" && "kind" in (result as object)) return { ...(result as Record<string, unknown>) };
  throw new TypeError("an action returns nothing, a message string or one of message, redirect, download and refresh");
}

/** The host function of one action (`action.<Resource>.<name>`). */
export function actionFunction(model: AnyModel, config: Loose, def: ActionDef, models: ReadonlyMap<string, AnyModel> = new Map()): HostFunction {
  const meta = model._meta;
  return async (database, args) => {
    const ctx = contextOf(database, { ...args, op: `action:${def.name}` }, meta, models);
    let result: unknown;
    try {
      result = await guarded(model, args, async () => await def.fn(ctx, querySet(model, database, args)));
    } catch (e) {
      const envelope = failure(meta, e);
      if (envelope === undefined) throw e;
      return envelope;
    }
    let shown: Record<string, unknown>;
    try {
      shown = resultJson(result);
    } catch (e) {
      return { ok: false, kind: "error", message: `the action ${def.name} of ${meta.name}: ${e instanceof Error ? e.message : String(e)}` };
    }
    return { ok: true, audited: ctx.audited, audit_keys: ctx.auditKeys, result: shown };
  };
}
