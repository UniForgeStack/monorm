/** Errors thrown by the ORM. */

/** Base class of every error the ORM throws (database errors included). */
export class ORMError extends Error {
  override name = "ORMError";
}

/** Reported by the database or the driver. */
export class DatabaseError extends ORMError {
  override name = "DatabaseError";
  /** The five-character SQLSTATE (`"23505"`); SQLite constraint failures get the
   * Postgres code too. */
  sqlstate: string | null = null;
  /** The violated constraint's name (Postgres only). */
  constraint: string | null = null;
  /** The database's DETAIL line (Postgres only). */
  detail: string | null = null;
}

/** Unique, foreign key, check, exclusion or not-null violation. */
export class IntegrityError extends DatabaseError {
  override name = "IntegrityError";
}

/** A row lock taken with `nowait` (or a lock timeout) found the rows locked. */
export class LockNotAvailable extends DatabaseError {
  override name = "LockNotAvailable";
}

/**
 * A statement or commit ran in a transaction that an earlier error aborted, also when
 * the code caught that error. `cause` is the first error. Catch errors only around a
 * savepoint (a nested `db.transaction()`).
 */
export class TransactionAborted extends DatabaseError {
  override name = "TransactionAborted";
}

/**
 * The deadline of a transaction (`timeout` of `db.transaction`) passed. `outcome` says
 * what is known: `"rolled_back"` after a confirmed rollback, or when no transaction began;
 * `"unknown"` when a COMMIT may have reached the server or the rollback is not confirmed.
 * `sqlstate` is the real server SQLSTATE if the server sent one, otherwise null.
 */
export class TransactionTimeoutError extends DatabaseError {
  override name = "TransactionTimeoutError";
  outcome: "rolled_back" | "unknown" | null = null;
}

/** The query does not match the schema, or asks for something unsupported. */
export class QueryError extends ORMError {
  override name = "QueryError";
}

/** The schema file or IR is invalid. */
export class SchemaError extends ORMError {
  override name = "SchemaError";
}

/** A stored nested value (JSON, or leaf columns) differs from its type; `path` says where. */
export class DecodeError extends ORMError {
  override name = "DecodeError";
  constructor(
    readonly model: string,
    readonly field: string,
    readonly path: readonly (string | number)[],
    detail: string,
  ) {
    super(`${model}.${[field, ...path].join(".")}: ${detail}`);
  }
}

/** The text of a `ValidationError`: the messages with no field, then `path: message`; the same text in every language. */
function validationText(fields: ReadonlyMap<string, readonly string[]>, nonFieldMessages: readonly string[]): string {
  const parts = [...nonFieldMessages, ...[...fields].flatMap(([path, list]) => list.map(m => `${path}: ${m}`))];
  return parts.length > 0 ? parts.join("; ") : "validation failed";
}

/**
 * App code rejects a value: messages per field path, and messages with no field.
 * A path joins field names and list indexes with dots: `address.city`, `lines.2.qty`.
 * A dot in a field name is not escaped, so the field `a.b` and the path `["a", "b"]` are one path.
 * `fields` is a `Map` in the order of first use; a plain object would move integer-like paths to the front.
 * `new ValidationError("text")` is one message with no field.
 */
export class ValidationError extends ORMError {
  override name = "ValidationError";
  readonly #fields = new Map<string, string[]>();
  readonly #nonFieldMessages: string[] = [];

  constructor(
    fields?: Readonly<Record<string, string | readonly string[]>> | string,
    options: { readonly nonFieldMessages?: readonly string[] } = {},
  ) {
    super("validation failed");
    if (typeof fields === "string") {
      this.#nonFieldMessages.push(fields);
    } else if (fields !== undefined) {
      for (const [path, texts] of Object.entries(fields)) {
        for (const text of typeof texts === "string" ? [texts] : texts) this.add(path, text);
      }
    }
    this.#nonFieldMessages.push(...(options.nonFieldMessages ?? []));
    this.#refresh();
  }

  /** The messages of each field path, in the order of first use. */
  get fields(): ReadonlyMap<string, readonly string[]> {
    return this.#fields;
  }

  /** The messages that belong to no field. */
  get nonFieldMessages(): readonly string[] {
    return this.#nonFieldMessages;
  }

  /** Add `message` to the field at `path` (a dotted string, or its parts). */
  add(path: string | readonly (string | number)[], message: string): this {
    const key = typeof path === "string" ? path : path.join(".");
    const list = this.#fields.get(key);
    if (list === undefined) this.#fields.set(key, [message]);
    else list.push(message);
    this.#refresh();
    return this;
  }

  /** Add a message that belongs to no field. */
  addNonFieldMessage(message: string): this {
    this.#nonFieldMessages.push(message);
    this.#refresh();
    return this;
  }

  /** `JSON.stringify` keeps the fields, which a `Map` alone would drop. */
  toJSON(): { name: string; message: string; fields: Record<string, string[]>; nonFieldMessages: string[] } {
    return {
      name: this.name,
      message: this.message,
      fields: Object.fromEntries([...this.#fields].map(([path, texts]) => [path, [...texts]])),
      nonFieldMessages: [...this.#nonFieldMessages],
    };
  }

  #refresh(): void {
    this.message = validationText(this.#fields, this.#nonFieldMessages);
  }
}

/** `get()` matched no row. Each model has a subclass: `User.DoesNotExist`. */
export class DoesNotExist extends ORMError {
  override name = "DoesNotExist";
}

/** `get()` matched more than one row. */
export class MultipleObjectsReturned extends ORMError {
  override name = "MultipleObjectsReturned";
}

/**
 * A relation was read on an instance without being loaded first. The types only allow
 * reading relations a query loaded (`load()`), so this is
 * thrown only past a cast.
 */
export class NotLoaded extends ORMError {
  override name = "NotLoaded";
}

/** No database: call `await connect(url)` first. */
export class NotConnected extends ORMError {
  override name = "NotConnected";
}

/** A lock was asked for outside `db.transaction()`, where it would be released as soon
 * as the statement ends. */
export class TransactionRequired extends ORMError {
  override name = "TransactionRequired";
}

/** The migrations directory and the database's migration history disagree. */
export class MigrationError extends ORMError {
  override name = "MigrationError";
}

/** An instance `update()` or `delete()` found a newer `@@locking.version` of its row:
 * another writer changed the row after it was loaded. */
export class VersionConflict extends ORMError {
  override name = "VersionConflict";
}

/** An ORM write to a `@@protected_write` model outside `allowWrites(...)`. */
export class WriteProtected extends ORMError {
  override name = "WriteProtected";
}

/** `update()` or `delete()` on a query set with no filter, which would change every row.
 * `.all()` marks a write to every row; `connect(url, { allowUnfilteredWrites: true })` turns the guard off. */
export class UnfilteredWriteError extends ORMError {
  override name = "UnfilteredWriteError";

  constructor(
    readonly op: string,
    readonly model: string,
  ) {
    super(
      `${model}.objects.${op}() has no filter and would change every row; call .all() first ` +
        `(${model}.objects.all().${op}(...)) to write every row, or open the database with connect(url, { allowUnfilteredWrites: true })`,
    );
  }
}

const KINDS: Record<string, new (message: string) => Error> = {
  MigrationError,
  DatabaseError,
  IntegrityError,
  LockNotAvailable,
  TransactionAborted,
  TransactionTimeoutError,
  QueryError,
  SchemaError,
  TypeError,
  ValidationError,
  WriteProtected,
};

/** The JSON line of a `[monorm:DatabaseError+]` error. */
interface NativeFields {
  sqlstate?: string | null;
  constraint?: string | null;
  detail?: string | null;
  outcome?: "rolled_back" | "unknown";
  cause?: NativeFields & { kind: string; message: string };
}

/** The JSON line of a `[monorm:ValidationError+]` error: the fields are pairs, in the order of first use. */
interface NativeValidation {
  fields?: [string, string[]][];
  nonFieldMessages?: string[];
}

function validationFromNative(native: NativeValidation): ValidationError {
  const out = new ValidationError(undefined, { nonFieldMessages: native.nonFieldMessages ?? [] });
  for (const [path, list] of native.fields ?? []) {
    for (const message of list) out.add(path, message);
  }
  return out;
}

function build(cls: new (message: string) => Error, message: string, fields: NativeFields): Error {
  const out = new cls(message);
  if (out instanceof DatabaseError) {
    out.sqlstate = fields.sqlstate ?? null;
    out.constraint = fields.constraint ?? null;
    out.detail = fields.detail ?? null;
  }
  if (out instanceof TransactionTimeoutError) {
    out.outcome = fields.outcome ?? null;
  }
  const cause = fields.cause;
  if (cause !== undefined) {
    out.cause = build(KINDS[cause.kind] ?? DatabaseError, cause.message, cause);
  }
  return out;
}

/** The error class for an error the native engine threw (`[monorm:<Kind>] message`). */
export function fromNative(e: unknown): unknown {
  if (!(e instanceof Error)) {
    return e;
  }
  const m = /^\[monorm:(\w+)(\+?)\] ([\s\S]*)$/.exec(e.message);
  const cls = m && KINDS[m[1]!];
  if (!m || !cls) {
    return e;
  }
  // `+`: a JSON line comes before the message: the fields of a database error, or the paths and messages of a validation error.
  let message = m[3]!;
  let json: unknown = {};
  if (m[2]) {
    const nl = message.indexOf("\n");
    json = JSON.parse(message.slice(0, nl));
    message = message.slice(nl + 1);
  }
  const out = cls === ValidationError ? validationFromNative(json as NativeValidation) : build(cls, message, json as NativeFields);
  if (e.stack) {
    const head = `${e.name}: ${e.message}`;
    const trace = e.stack.startsWith(head) ? e.stack.slice(head.length) : "";
    out.stack = `${out.name}: ${message}${trace}`;
  }
  return out;
}
