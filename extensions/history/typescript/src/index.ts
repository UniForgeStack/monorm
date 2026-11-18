import { allowWrites, Decimal, getDatabase } from "monorm";
import type { Database, ModelClass, ModelSpec } from "monorm";
import { canonicalHash, databaseOf, jsonDiff, parseTime } from "monorm/ext";
import type { QuerySetCtor } from "monorm/ext";

const SOURCE = "monorm.history";
const RESTORE = "restore";
const RECORD_TYPES = ["create", "update", "delete"];
// A restore or a delete is a fact, so it is written even when the snapshot has the hash of the latest version.
const ALWAYS_WRITTEN = ["delete", RESTORE];

type Any = any; // eslint-disable-line @typescript-eslint/no-explicit-any

/** The model metadata of an instance; the host names it `meta` or `_meta` by build. */
function metaOf(value: unknown): Meta | undefined {
  const ctor = (value as { constructor?: { meta?: Meta; _meta?: Meta } } | null | undefined)?.constructor;
  return ctor?.meta ?? ctor?._meta;
}
type Json = Record<string, unknown>;
type Meta = Any;

/** One event of a timeline. `diff` is `{field: {old, new}}` against the previous event of the row; `null` for the first event and when `diff: true` was not asked. */
export interface HistoryEntry {
  readonly event: Any;
  readonly diff: Record<string, { old: unknown; new: unknown }> | null;
}

export interface TrackerOption {
  /** The event model name, when the model has several trackers. */
  readonly tracker?: string;
}

interface Tracker {
  readonly event_model: string;
  readonly event_table: string;
  readonly key: readonly { field: string; column: string; event_field: string; event_column: string }[];
  readonly full: boolean;
  readonly event_sql: string;
  readonly backfill_sql: string;
  /** PostgreSQL: the SQL that sets the restore setting; `null` on SQLite. */
  readonly restore_sql: string | null;
}

interface VersionModel {
  readonly of_field: string;
  readonly pk_field: string;
  /** PostgreSQL: the SQL of the numbering lock; `null` on SQLite. */
  readonly lock_sql: string | null;
  readonly latest_sql: string;
  readonly insert_sql: string;
}

/** The snapshot function and the restore function of a version model; each may be sync or async. */
export interface VersionFunctions<E = Any> {
  readonly generate: (entity: E) => Json | Promise<Json>;
  readonly restore: (snapshot: Json) => unknown;
}

const REGISTERED = new WeakMap<object, VersionFunctions>();

/** The value of a JSON value that an event holds: dates, times, decimals and big integers are text or numbers in JSON. */
function decode(type: string, value: unknown, field: string): unknown {
  if (value === null || value === undefined || type === "json") return value;
  if (Array.isArray(value)) return value.map((v) => decode(type, v, field));
  if (typeof value === "string") {
    // A naive text (SQLite) is UTC, as the engine writes it.
    if (type === "date_time") return new Date(/(Z|[+-]\d\d(:?\d\d)?)$/.test(value) ? value.replace(" ", "T") : `${value.replace(" ", "T")}Z`);
    if (type === "date") return new Date(`${value}T00:00:00Z`);
    if (type === "time") return parseTime(value) ?? value;
    if (type === "big_int" && /^-?\d+$/.test(value)) return BigInt(value);
  }
  if (type === "big_int" && typeof value === "number") {
    // The JSON number of an event lost the digits above 2^53 when it was read; do not write the wrong value back.
    if (!Number.isSafeInteger(value)) throw new Error(`cannot restore ${field}: the event holds ${value}, a JSON number above 2^53 that lost precision`);
    return BigInt(value);
  }
  if (type === "decimal" && !(value instanceof Decimal)) return new Decimal(String(value));
  return value;
}

/** `Model.history` of a model with `@@history.track`: the events of its trackers. */
export class History {
  constructor(private readonly meta: Meta, private readonly db?: Database) {}

  /** The same API on the database `db`. */
  using(db: Database | undefined): History {
    return new History(this.meta, db);
  }

  private trackers(): Tracker[] {
    const records = this.meta.registry.behaviorRecords("extension_records", this.meta.name) as Json[];
    return records.filter((r) => r["source"] === SOURCE && r["kind"] === "tracker").map((r) => r["data"] as Tracker);
  }

  private tracker(name: string | undefined): Tracker {
    const all = this.trackers();
    if (all.length === 0) throw new TypeError(`${this.meta.name} has no @@history.track attribute`);
    if (name === undefined) {
      if (all.length > 1) throw new Error(`${this.meta.name} has several trackers (${all.map((t) => t.event_model).join(", ")}); pass tracker: <event model name>`);
      return all[0]!;
    }
    const found = all.find((t) => t.event_model === name);
    if (found === undefined) throw new Error(`${this.meta.name} has no tracker with the event model ${name}`);
    return found;
  }

  private eventModel(t: Tracker): Any {
    return this.meta.registry.get(t.event_model).model;
  }

  private key(row: unknown): unknown[] {
    const fields: Any[] = this.meta.key;
    let values: unknown[];
    if (Array.isArray(row)) values = row;
    else if (row !== null && typeof row === "object") values = fields.map((f) => (row as Json)[f.name]);
    else values = [row];
    if (values.length !== fields.length) throw new Error(`${this.meta.name} has the key (${fields.map((f) => f.name).join(", ")}): got ${values.length} value(s)`);
    if (values.some((v) => v === null || v === undefined)) throw new Error(`${this.meta.name}: the key of the row has a null value; save the row first`);
    return values;
  }

  private database(row?: unknown): Database {
    return this.db ?? (row !== null && typeof row === "object" ? databaseOf(row) : undefined) ?? getDatabase();
  }

  private conditions(t: Tracker, event: Any, key: readonly unknown[]): Any[] {
    return t.key.map((k, i) => event._meta.column(event._meta.fieldByIr.get(k.event_field)).eq(key[i]));
  }

  /** Write one event of `row` now, with the label `label`. Gives the event. */
  async event(row: unknown, label: string, options: TrackerOption = {}): Promise<Any> {
    const t = this.tracker(options.tracker);
    const db = this.database(row);
    const key = this.key(row);
    const rows = await db.fetch(t.event_sql, label, ...key);
    if (rows.length === 0) throw new Error(`${this.meta.name} ${key.length === 1 ? String(key[0]) : JSON.stringify(key)} does not exist`);
    const event = this.eventModel(t);
    return await event.objects.using(db).filter(event.id.eq(rows[0]!["id"])).first();
  }

  /** Write one event per row of the tracked table, in one statement. Gives the number of events. */
  async backfill(label = "snapshot", options: TrackerOption = {}): Promise<number> {
    const t = this.tracker(options.tracker);
    const rows = await this.database().fetch(t.backfill_sql, label);
    // PostgreSQL gives one row with the count; SQLite gives one row per event.
    return rows.length > 0 && "n" in rows[0]! ? Number(rows[0]!["n"]) : rows.length;
  }

  /** Delete the events older than `before`: the one delete path of an event model. Without `tracker`, it purges every tracker of the model. Gives the number of events deleted. */
  async purge(options: TrackerOption & { readonly before: Date }): Promise<number> {
    const db = this.database();
    let total = 0;
    for (const t of options.tracker !== undefined ? [this.tracker(options.tracker)] : this.trackers()) {
      const event = this.eventModel(t);
      total += Number(await allowWrites([event], async () => await event.objects.using(db).filter(event.createdAt.lt(options.before)).delete()));
    }
    return total;
  }

  /** The events of `row` in order. With `diff: true`, each entry has the changed fields against the previous event of the row. */
  async timeline(row: unknown, options: TrackerOption & { readonly diff?: boolean } = {}): Promise<HistoryEntry[]> {
    if (this.versionModel() !== null) return await this.versionTimeline(row, options.diff === true);
    const t = this.tracker(options.tracker);
    const event = this.eventModel(t);
    const events = (await event.objects.using(this.database(row)).filter(...this.conditions(t, event, this.key(row))).orderBy(event.id.asc())) as Any[];
    const entries: HistoryEntry[] = [];
    let previous: Any = null;
    for (const e of events) {
      const diff = options.diff === true && previous !== null ? jsonDiff(previous.data as Json, e.data as Json) : null;
      entries.push({ event: e, diff });
      previous = e;
    }
    return entries;
  }

  /** The last event of `row` at or before `when`, or `null`. Its `data` is the row state then. */
  async at(row: unknown, when: Date, options: TrackerOption = {}): Promise<Any> {
    if (this.versionModel() !== null) return await this.versionAt(row, when);
    const t = this.tracker(options.tracker);
    const event = this.eventModel(t);
    return await event.objects.using(this.database(row)).filter(...this.conditions(t, event, this.key(row)), event.createdAt.lte(when)).orderBy(event.id.desc()).first();
  }

  /**
   * Write the row back from `event`, in one transaction, and leave one event with the label `restore`.
   * A row that exists is updated with the fields of the event. A deleted row is inserted again, only when
   * the tracker covers every field. Gives the row.
   */
  async restore(event: Any): Promise<Any> {
    if (this.versionModel() !== null) return await this.restoreVersion(event);
    const eventMeta = metaOf(event) as Meta;
    const t = this.trackers().find((x) => x.event_model === eventMeta.name);
    if (t === undefined) throw new TypeError(`${String(eventMeta.name)} is not the event model of a tracker of ${this.meta.name}`);
    const meta = this.meta;
    const db = this.database(event);
    const key = t.key.map((k) => event[eventMeta.fieldByIr.get(k.event_field).name]);
    const keyIr = new Set(t.key.map((k) => k.field));
    const values: Json = {};
    const names = new Set<string>(meta.inputFields.keys()); // the camelCase names of the input fields
    for (const [irName, value] of Object.entries(event.data as Json)) {
      const f = meta.fieldByIr.get(irName);
      if (f !== undefined && names.has(f.name)) values[f.name] = decode(f.type, value, f.name);
    }
    t.key.forEach((k, i) => {
      const f = meta.fieldByIr.get(k.field);
      if (!(f.name in values)) values[f.name] = key[i];
    });
    const model = meta.model as Any;
    const match = meta.keyMatch(meta.key, key.length === 1 ? key[0] : Object.fromEntries(meta.key.map((f: Any, i: number) => [f.name, key[i]])));
    return await db.transaction(async () => {
      // Bypass read defaults (soft delete, tenant) so a hidden row is not inserted twice.
      const rows = model.objects.using(db).withoutDefaults();
      const found = await rows.filter(match).first();
      const after = await this.lastId(db, eventMeta, t, key);
      await this.markRestore(db, t, t.event_table);
      let row: Any;
      if (found !== null) {
        const changes = Object.fromEntries(Object.entries(values).filter(([n]) => !keyIr.has(meta.fields.get(n)?.ir)));
        if (t.full) this.checkRequired(values, event);
        if (Object.keys(changes).length > 0) await rows.filter(match).update(changes);
        row = await rows.filter(match).first();
      } else {
        if (!t.full) {
          throw new Error(
            `cannot restore ${meta.name} from ${eventMeta.name} ${String(event.id)}: the row is gone and the tracker covers only some fields, so an insert would write a half row`,
          );
        }
        this.checkRequired(values, event);
        row = await rows.insert(values);
      }
      await this.markRestore(db, t, "");
      await this.labelRestore(db, eventMeta, t, key, after, row);
      return row;
    });
  }

  // ---- versions mode ----

  private versionModel(): VersionModel | null {
    const records = this.meta.registry.behaviorRecords("extension_records", this.meta.name) as Json[];
    const found = records.find((r) => r["source"] === SOURCE && r["kind"] === "versions");
    return found === undefined ? null : (found["data"] as VersionModel);
  }

  private versions(): VersionModel {
    const v = this.versionModel();
    if (v === null) throw new TypeError(`${this.meta.name} has no @@history.versions attribute`);
    return v;
  }

  private functions(): VersionFunctions {
    const found = REGISTERED.get(this.meta);
    if (found === undefined) {
      const name = String(this.meta.name);
      throw new Error(`${name}.history is not registered; call history(${name}).register({ generate, restore }) first`);
    }
    return found;
  }

  /** Set the two functions of this version model. `generate(entity)` gives the snapshot, an object of JSON values; `restore(snapshot)` writes it back. Each may be sync or async. */
  register(functions: VersionFunctions): void {
    this.versions();
    if (typeof functions?.generate !== "function" || typeof functions?.restore !== "function") throw new TypeError("register needs generate and restore functions");
    REGISTERED.set(this.meta, { generate: functions.generate, restore: functions.restore });
  }

  /** The id of the entity: a plain value is the id; an object gives its `id`. */
  private entityId(entity: unknown): unknown {
    if (entity === null || entity === undefined) throw new Error("the entity has no id: pass an id value, or an object with an id");
    if (typeof entity !== "object" || entity instanceof Date) return entity;
    const id = (entity as Json)["id"];
    if (id === null || id === undefined) throw new Error("the entity has no id: pass an id value, or an object with an id");
    return id;
  }

  private versionField(irName: string): Any {
    return (this.meta.model as Any)[this.meta.fieldByIr.get(irName).name];
  }

  private async write(db: Database, v: VersionModel, entityId: unknown, makeSnapshot: () => Json | Promise<Json>, changeType: string, restoredFrom: unknown): Promise<Any> {
    const model = this.meta.model as Any;
    const id = this.versionField(v.pk_field);
    return await db.transaction(async () => {
      // The lock comes before the snapshot and the read of the latest version, so a second writer waits for the first one and never writes an older snapshot after a newer one.
      if (v.lock_sql !== null) await db.fetch(v.lock_sql, entityId);
      const snapshot = await makeSnapshot();
      if (snapshot === null || typeof snapshot !== "object" || Array.isArray(snapshot)) throw new TypeError("generate must give an object");
      const digest = canonicalHash(snapshot);
      if (!ALWAYS_WRITTEN.includes(changeType)) {
        const latest = await db.fetch(v.latest_sql, entityId);
        // A version after a delete is a new life of the entity, so it never matches the delete version.
        if (latest.length > 0 && latest[0]!["change_type"] !== "delete" && latest[0]!["content_hash"] === digest) return await model.objects.using(db).filter(id.eq(latest[0]!["id"])).first();
      }
      const rows = await db.fetch(v.insert_sql, entityId, snapshot, digest, changeType, restoredFrom);
      return await model.objects.using(db).filter(id.eq(rows[0]!["id"])).first();
    });
  }

  /** Store the snapshot that `generate(entity)` gives as the next version of the entity, with the change type `create`, `update` or `delete`. A snapshot with the hash of the latest version writes nothing and gives that version, unless that version is a `delete`. Inside a transaction, the version rolls back with it. Gives the version. */
  async record(entity: unknown, changeType: "create" | "update" | "delete"): Promise<Any> {
    const v = this.versions();
    if (!RECORD_TYPES.includes(changeType)) throw new Error(`changeType must be one of ${RECORD_TYPES.join(", ")}; restore() writes the restore version`);
    const { generate } = this.functions();
    return await this.write(this.database(entity), v, this.entityId(entity), () => generate(entity), changeType, null);
  }

  private async versionTimeline(entity: unknown, diff: boolean): Promise<HistoryEntry[]> {
    const v = this.versions();
    const model = this.meta.model as Any;
    const versions = (await model.objects
      .using(this.database(entity))
      .filter(this.versionField(v.of_field).eq(this.entityId(entity)))
      .orderBy(model.versionNumber.asc())) as Any[];
    const entries: HistoryEntry[] = [];
    let previous: Any = null;
    for (const version of versions) {
      entries.push({ event: version, diff: diff && previous !== null ? jsonDiff(previous.snapshot as Json, version.snapshot as Json) : null });
      previous = version;
    }
    return entries;
  }

  /** The last version of the entity with `effectiveAt` at or before `when`, or `null`. */
  async versionAt(entity: unknown, when: Date): Promise<Any> {
    const v = this.versions();
    const model = this.meta.model as Any;
    return await model.objects
      .using(this.database(entity))
      .filter(this.versionField(v.of_field).eq(this.entityId(entity)), model.effectiveAt.lte(when))
      .orderBy(model.versionNumber.desc())
      .first();
  }

  private async restoreVersion(version: Any): Promise<Any> {
    const v = this.versions();
    if (metaOf(version) !== this.meta) throw new TypeError(`the value is not a version of ${this.meta.name}`);
    const { restore } = this.functions();
    const db = this.database(version);
    const field = (irName: string): unknown => version[this.meta.fieldByIr.get(irName).name];
    return await db.transaction(async () => {
      await restore(version.snapshot as Json);
      return await this.write(db, v, field(v.of_field), () => version.snapshot as Json, RESTORE, field(v.pk_field));
    });
  }

  private checkRequired(values: Json, event: Any): void {
    const missing: string[] = [];
    for (const [name, f] of this.meta.inputFields as Map<string, Any>) {
      if (!(name in values) && !f.nullable && !f.hasInsertDefault) missing.push(name);
    }
    if (missing.length > 0) {
      throw new Error(`cannot restore ${this.meta.name} from event ${String(event.id)}: the event has no value for the required field(s) ${missing.join(", ")}`);
    }
  }

  private async lastId(db: Database, eventMeta: Meta, t: Tracker, key: readonly unknown[]): Promise<number> {
    const event = eventMeta.model as Any;
    const last = await event.objects.using(db).filter(...this.conditions(t, event, key)).orderBy(event.id.desc()).first();
    return last === null ? 0 : Number(last.id);
  }

  /** PostgreSQL: set the transaction-local setting that makes the triggers of the tracker write the label `restore`; an empty `value` clears it. SQLite has no such SQL. */
  private async markRestore(db: Database, t: Tracker, value: string): Promise<void> {
    if (t.restore_sql !== null) await db.fetch(t.restore_sql, value);
  }

  /** Leave one `restore` event: PostgreSQL triggers wrote it already; on SQLite the events after `after` get the label here. Write one when no trigger did. */
  private async labelRestore(db: Database, eventMeta: Meta, t: Tracker, key: readonly unknown[], after: number, row: Any): Promise<void> {
    const event = eventMeta.model as Any;
    const where = [...this.conditions(t, event, key), event.id.gt(after)];
    const found =
      t.restore_sql === null
        ? Number(await allowWrites([event], async () => await event.objects.using(db).filter(...where).update({ label: RESTORE })))
        : Number(await event.objects.using(db).filter(...where, event.label.eq(RESTORE)).count());
    if (found === 0) await this.using(db).event(row, RESTORE, { tracker: t.event_model });
  }
}

/** `history(Post)`: the history API of a model with `@@history.track`. */
export function history(model: ModelClass<ModelSpec>): History {
  return new History((model as Any)._meta);
}

/** No members: the history API is `history(Model)` and `row.history`. */
export function historyQuerySet<B extends QuerySetCtor>(Base: B) {
  return class HistoryQuerySet extends Base {};
}

/** The instance mixin: `row.history`. */
export class HistoryInstance {
  /** The history API of this row's model, on the database that loaded the row. */
  get history(): History {
    return new History(metaOf(this) as Meta, databaseOf(this as object));
  }
}
