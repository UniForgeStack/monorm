/**
 * OpenTelemetry spans for the queries of a database: `instrument(db)`.
 *
 * Loads `@opentelemetry/api` (an optional peer dependency) only when no tracer is
 * given; the package imports it nowhere else.
 */

import type { Database, QueryEvent } from "./db.js";

/** The span the ORM uses: a subset of the OpenTelemetry `Span`. */
export interface Span {
  setStatus(status: { code: number; message?: string }): unknown;
  end(endTime?: number): void;
}

/** The tracer the ORM uses: a subset of the OpenTelemetry `Tracer`. */
export interface Tracer {
  startSpan(name: string, options?: { kind?: number; attributes?: Record<string, string | number>; startTime?: number }): Span;
}

export interface InstrumentOptions {
  /** The tracer of the spans (default: `trace.getTracer("monorm")` of `@opentelemetry/api`). */
  readonly tracer?: Tracer;
  /** Keep `db.query.text` for `db.execute(sql)`. Its SQL takes no parameters, so it can hold
   * literal values: the spans omit the text unless this is `true`. */
  readonly rawSql?: boolean;
}

// The values of the OpenTelemetry API's SpanKind.CLIENT and SpanStatusCode.ERROR.
const CLIENT = 2;
const ERROR = 2;
const MAX_TEXT = 4096;

/**
 * Gives each statement of `db` a client span; resolves to a function that stops it.
 *
 * The span starts and ends at the statement's own times and has the active span of the
 * calling async context as its parent. Its name is the SQL operation (`SELECT`), and it
 * has `db.system.name`, `db.operation.name`, `db.query.text` (the SQL with placeholders; cut at
 * 4096 characters then `...`) and `db.response.returned_rows`. A failed statement gets the error
 * status and the database's message.
 *
 * `db.execute(sql)` takes no parameters, so its SQL can hold literal values: its spans have no
 * `db.query.text` unless `rawSql: true`. ORM-built SQL and `db.fetch(sql, ...params)` always keep
 * the text.
 */
export async function instrument(db: Database, options: InstrumentOptions = {}): Promise<() => void> {
  const tracer = options.tracer ?? (await defaultTracer());
  const system = db.url.startsWith("sqlite:") ? "sqlite" : "postgresql";
  return db.onQuery((e: QueryEvent) => {
    const operation = /^[\s(]*(\w+)/.exec(e.sql)?.[1]?.toUpperCase() ?? "SQL";
    const attributes: Record<string, string | number> = {
      "db.system.name": system,
      "db.operation.name": operation,
      "db.response.returned_rows": e.rows,
    };
    if (options.rawSql === true || !e.raw) attributes["db.query.text"] = e.sql.length <= MAX_TEXT ? e.sql : `${e.sql.slice(0, MAX_TEXT)}...`;
    const span = tracer.startSpan(operation, { kind: CLIENT, attributes, startTime: e.start });
    if (e.error !== null) span.setStatus({ code: ERROR, message: e.error });
    span.end(e.start + e.duration);
  });
}

async function defaultTracer(): Promise<Tracer> {
  // A variable specifier: the type check and the build work without the package.
  const name = "@opentelemetry/api";
  let api: { trace: { getTracer(name: string): Tracer } };
  try {
    api = (await import(name)) as typeof api;
  } catch (e) {
    throw new Error("monorm/otel needs @opentelemetry/api: npm install @opentelemetry/api", { cause: e });
  }
  return api.trace.getTracer("monorm");
}
