/**
 * Custom admin routes: the host functions of `Admin.route` and of the streams that a route returns.
 *
 * Rust runs the session, the CSRF check and the permission check, then calls `route.<METHOD>.<path>` with the request.
 * A route that returns a stream gets an id. Rust then calls `route.__next` once per chunk (the client reads at its own pace)
 * and `route.__close` at the end, so no layer holds the whole body.
 */
import { Context } from "./admin-write.js";
import type { HostFunction } from "./admin-host.js";

export const NEXT = "route.__next";
export const CLOSE = "route.__close";
const COALESCE = 16 * 1024;

/** The request that a route function gets: `params` are the `{name}` parts of the path, `query` maps a query name to a string (a list when the name repeats), and `body` is the JSON body (`null` when empty or on a GET). */
export interface RouteRequest {
  readonly method: string;
  readonly path: string;
  readonly params: Readonly<Record<string, string>>;
  readonly query: Readonly<Record<string, string | string[]>>;
  readonly body: unknown;
}

/** A JSON answer with its own status code (200 by default). */
export class Reply {
  readonly status: number;
  constructor(
    readonly json: unknown = null,
    options: { readonly status?: number } = {},
  ) {
    this.status = options.status ?? 200;
  }
}

export type StreamChunk = Uint8Array | string;

/**
 * A body that the admin sends chunk by chunk. `chunks` is an iterable or an async iterable of bytes or strings.
 * `filename` makes the browser save it. A route function that fails after the first chunk cuts the response.
 */
export class Stream {
  readonly contentType: string;
  readonly filename: string | null;
  readonly status: number;
  constructor(
    readonly chunks: Iterable<StreamChunk> | AsyncIterable<StreamChunk>,
    options: { readonly contentType?: string; readonly filename?: string; readonly status?: number } = {},
  ) {
    this.contentType = options.contentType ?? "application/octet-stream";
    this.filename = options.filename ?? null;
    this.status = options.status ?? 200;
  }
}

export type RouteFunction = (ctx: Context, request: RouteRequest) => unknown;

function jsonable(value: unknown): unknown {
  const text = JSON.stringify(value ?? null, function (this: any, key: string, v: unknown) {
    const raw = this[key];
    if (raw instanceof Uint8Array) return Buffer.from(raw).toString("base64");
    if (typeof v === "bigint") return Number.isSafeInteger(Number(v)) ? Number(v) : v.toString();
    return v;
  });
  return JSON.parse(text);
}

function bytes(piece: unknown): Buffer {
  if (typeof piece === "string") return Buffer.from(piece);
  if (piece instanceof Uint8Array) return Buffer.from(piece);
  throw new TypeError(`a stream yielded ${typeof piece}: yield bytes or a string`);
}

class OpenStream {
  readonly #async: AsyncIterator<StreamChunk> | null;
  readonly #sync: Iterator<StreamChunk> | null;

  constructor(chunks: Iterable<StreamChunk> | AsyncIterable<StreamChunk>) {
    if (Symbol.asyncIterator in chunks) {
      this.#async = (chunks as AsyncIterable<StreamChunk>)[Symbol.asyncIterator]();
      this.#sync = null;
    } else {
      this.#async = null;
      this.#sync = (chunks as Iterable<StreamChunk>)[Symbol.iterator]();
    }
  }

  /** The next bytes, or `null` at the end. A sync iterator joins small chunks up to 16 KiB; an async iterator gives one chunk per pull. */
  async next(): Promise<Buffer | null> {
    if (this.#async !== null) {
      const step = await this.#async.next();
      return step.done === true ? null : bytes(step.value);
    }
    const parts: Buffer[] = [];
    let size = 0;
    for (;;) {
      const step = this.#sync!.next();
      if (step.done === true) break;
      const part = bytes(step.value);
      parts.push(part);
      size += part.length;
      if (size >= COALESCE) break;
    }
    return parts.length === 0 ? null : Buffer.concat(parts);
  }

  async close(): Promise<void> {
    if (this.#async !== null) await this.#async.return?.();
    else this.#sync!.return?.();
  }
}

let nextId = 1;

class Streams {
  readonly #open = new Map<string, OpenStream>();

  open(chunks: Iterable<StreamChunk> | AsyncIterable<StreamChunk>): string {
    const id = String(nextId++);
    this.#open.set(id, new OpenStream(chunks));
    return id;
  }

  readonly next: HostFunction = async (_database, args) => {
    const stream = this.#open.get(args.stream);
    if (stream === undefined) return { chunk: null };
    const chunk = await stream.next();
    return { chunk: chunk === null ? null : chunk.toString("base64") };
  };

  readonly close: HostFunction = async (_database, args) => {
    const stream = this.#open.get(args.stream);
    this.#open.delete(args.stream);
    await stream?.close();
    return null;
  };
}

function routeFunction(fn: RouteFunction, streams: Streams): HostFunction {
  return async (database, args) => {
    const ctx = new Context({ user: args.user, requestId: args.request_id ?? "", database, ip: args.ip, userAgent: args.user_agent });
    const request: RouteRequest = { method: args.method, path: args.path, params: args.params ?? {}, query: args.query ?? {}, body: args.body ?? null };
    const result = await fn(ctx, request);
    if (result instanceof Stream) {
      const out: Record<string, unknown> = { stream: streams.open(result.chunks), content_type: result.contentType, status: result.status };
      if (result.filename !== null) out["filename"] = result.filename;
      return out;
    }
    if (result instanceof Reply) return { json: jsonable(result.json), status: result.status };
    return { json: jsonable(result) };
  };
}

/** The host entries of a set of routes, keyed by `[METHOD, path]`, with their stream functions. */
export function routeHost(routes: Iterable<readonly [string, string, RouteFunction]>): Record<string, HostFunction> {
  const streams = new Streams();
  const out: Record<string, HostFunction> = {};
  for (const [method, path, fn] of routes) out[`route.${method}.${path}`] = routeFunction(fn, streams);
  out[NEXT] = streams.next;
  out[CLOSE] = streams.close;
  return out;
}
