// The admin protocol, and nothing else: the SPA reads no other server path.
import { BASE } from "./router";
import type { ActionInfo, Brand, Column, Filter, FormField, InlineInfo, InputInfo, NamedFilter, RelationInfo, Resource, Section, Switcher, SwitcherService, Tab } from "./generated/api-types";

/** The metadata types come from the Rust structs (`crates/admin/src/wire.rs`): `scripts/gen-admin-types.py` writes them. */
export type { ActionInfo, Brand, Column, Filter, FormField, InlineInfo, InputInfo, NamedFilter, RelationInfo, Resource, Section, Switcher, SwitcherService, Tab };

export type ActionVariant = NonNullable<ActionInfo["variant"]>;

/** What an action answers: the SPA shows the message, follows the redirect, saves the download or reloads the data. */
export type ActionResult =
  | { kind: "message"; message: string; level: "info" | "success" | "warning" | "error" }
  | { kind: "redirect"; to: string }
  | { kind: "download"; filename: string; content_type: string; base64: string }
  | { kind: "refresh" };

/** The rows of an action: the keys of a selection, or all the rows that the URL filters match. An input is a text, a flag or a file as `{name, content_type, base64}`. */
export interface ActionBody {
  keys?: string[];
  all?: boolean;
  inputs?: Values;
  confirm_large?: boolean;
}

/** `search` carries the filters of the list: an action on "all rows" takes them from the URL. `scope` runs it on a to-many tab. */
export function runAction(resource: string, action: string, body: ActionBody, search = "", scope?: Scope) {
  const path = scope
    ? `/tab-actions/${encodeURIComponent(scope.resource)}/${scope.key}/${encodeURIComponent(scope.rel)}/${encodeURIComponent(action)}`
    : `/actions/${encodeURIComponent(resource)}/${encodeURIComponent(action)}`;
  return call<{ count: number; capped: boolean; affected?: number; result: ActionResult }>("POST", `${path}${search}`, body);
}

/** Whether the user may do the write. The server checks it again. */
export const may = (r: Resource | undefined, op: "create" | "update" | "delete") => !!r && r.writable !== false && !!r.form && (r.ops ?? [op]).includes(op);

export interface Nav {
  group: string | null;
  icon?: string | null;
  resources: string[];
}

/** An extension package: its widgets, form modules and styles, as paths under the API prefix (`packages/<package>/<file>`). */
export interface PackageInfo {
  name: string;
  admin_major: number;
  widgets: Record<string, string>;
  forms: Record<string, string>;
  tabs: Record<string, string>;
  styles: string[];
}

export interface Site {
  brand: Brand;
  nav: Nav[];
  resources: Resource[];
  /** The major version of the widget contract of this admin. */
  admin_major: number;
  packages: PackageInfo[];
  frame_cards?: Frame[];
}

/** One flag for each `can` rule of the resource: whether the rule holds for the row. */
export type Flags = Record<string, boolean>;

export interface Page {
  resource: string;
  columns: Column[];
  rows: unknown[][];
  keys: string[];
  next: string | null;
  prev: string | null;
  flags?: Flags[];
  /** For each row, the label of the related row of each foreign-key column (by column label). A missing entry shows the raw value. */
  labels?: Record<string, unknown>[];
  /** Present when a custom search ran: the count of that one call, always exact. */
  count?: number;
  capped?: boolean;
}

/** A to-many tab: the children of one record of `resource` (`key` is its encoded path segment) through the relation `rel`. */
export interface Scope {
  resource: string;
  key: string;
  rel: string;
}

export interface Count {
  count: number;
  capped: boolean;
}

export interface Frame {
  name: string;
  title: string;
  url: string | null;
  sandbox: string;
  height: string;
  referrer_policy: "same-origin" | "no-referrer";
}

export interface Embed {
  title: string;
  key: string;
  tab: {
    fields: Column[];
    values: unknown[];
    name?: string;
    resource?: string;
    package?: Pick<PackageInfo, "name" | "tabs" | "styles"> & { admin_major: number };
  };
}

export interface Detail {
  resource: string;
  key: string;
  fields: Column[];
  values: unknown[];
  flags?: Flags;
  links: { name: string; title: string; resource: string; key: string }[];
  /** The rows of each inline of the record. */
  inlines?: Record<string, InlineRows>;
  frames?: Frame[];
  /** The banners of the record: one host call per read. */
  banners?: { level: "info" | "warning" | "danger"; text: string }[];
  password_target?: { display: string | null; sso_only: boolean };
}

export interface InlineRows {
  rows: { key: string; values: unknown[]; flags?: Flags }[];
  truncated: boolean;
  link: string | null;
}

/** The inline rows of a parent save, for each inline. An update row and a delete row name the row by its encoded key. */
export type InlineChanges = Record<string, { create: { set: Values }[]; update: { key: string; set: Values; loaded: Values }[]; delete: { key: string; loaded: Values }[] }>;

export interface User {
  key: unknown;
  username?: string;
  display?: string;
}

/** The parts of an error answer that a form shows: a message for each field, messages for the form, and the 409 values. */
export interface ErrorBody {
  error?: { errors?: { code: string; message: string }[] };
  fields?: Record<string, string[]>;
  form?: string[];
  conflicts?: Record<string, { loaded: unknown; current: unknown }>;
}

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly kind: string,
    message: string,
    readonly body: ErrorBody = {},
  ) {
    super(message);
  }
}

const base = document.querySelector<HTMLMetaElement>('meta[name="monorm-admin-api"]')?.content ?? `${BASE}/api`;
let csrf = "";

/** The token of the shared cookie at this moment: another tab may have renewed it since this tab loaded. The last `/me` answer is the fallback for a cookie that script cannot read. */
function csrfToken(): string {
  for (const part of document.cookie.split(";")) {
    const [name, ...value] = part.trim().split("=");
    if (name === "__Secure-monorm-csrf") return value.join("=");
  }
  return csrf;
}

async function call<T>(method: string, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = {};
  // A `FormData` body is the upload: the browser sets the multipart content type with its boundary.
  const upload = body instanceof FormData;
  if (body !== undefined && !upload) headers["content-type"] = "application/json";
  if (method !== "GET") headers["x-csrf-token"] = csrfToken();
  const response = await fetch(`${base}${path}`, {
    method,
    headers,
    credentials: "same-origin",
    body: body === undefined ? undefined : upload ? body : JSON.stringify(body),
  });
  const text = await response.text();
  let data: { ok?: boolean; error?: { kind: string; message: string }; csrf?: string } & ErrorBody = {};
  try {
    data = text ? JSON.parse(text) : {};
  } catch {
    throw new ApiError(response.status, "BadResponse", `the server answered ${response.status} with no JSON`);
  }
  if (!response.ok || data.ok === false) {
    throw new ApiError(response.status, data.error?.kind ?? "Error", data.error?.message ?? `the server answered ${response.status}`, data);
  }
  if (data.csrf) csrf = data.csrf;
  return data as T;
}

export interface Session {
  authenticated: boolean;
  user: User | null;
  /** The address that starts the single sign-on, when the site has one. */
  sso: string | null;
  /** The site sends a visitor with no session to the single sign-on at once. */
  autoStart: boolean;
}

/** The login state: the token for writes, and the user when a session exists. */
export async function session(): Promise<Session> {
  const state = await call<{ authenticated: boolean; sso?: string; auto_start?: boolean }>("GET", "/login");
  if (!state.authenticated) return { authenticated: false, user: null, sso: state.sso ?? null, autoStart: state.auto_start === true };
  const me = await call<{ user: User }>("GET", "/me");
  return { authenticated: true, user: me.user, sso: null, autoStart: false };
}

export const login = (username: string, password: string) => call<{ user: User }>("POST", "/login", { username, password });
export const logout = () => call<object>("POST", "/logout", {});
export const changePassword = (old_password: string, new_password: string) => call<object>("POST", "/password", { old_password, new_password });
/** The URL of a package file: the path that `/site` gives, under the API prefix. */
export const packageUrl = (path: string) => `${base}/${path}`;

/** A custom route (`<prefix>/routes/<path>`) with the session and the CSRF token. The answer is the raw response: a route is not an admin call. */
export async function routeFetch(method: string, path: string, body?: unknown): Promise<Response> {
  const headers: Record<string, string> = {};
  if (body !== undefined) headers["content-type"] = "application/json";
  if (method !== "GET") headers["x-csrf-token"] = csrfToken();
  return fetch(`${base}/routes/${path.replace(/^\/+/, "")}`, { method, headers, credentials: "same-origin", body: body === undefined ? undefined : JSON.stringify(body) });
}

/** The JSON answer of a custom route. A status of 400 or more throws with the status and the text of the answer. */
export async function routeJson(method: string, path: string, body?: unknown): Promise<unknown> {
  const response = await routeFetch(method, path, body);
  const text = await response.text();
  if (!response.ok) throw new Error(`${method} ${path}: ${response.status} ${text}`);
  return text === "" ? null : JSON.parse(text);
}

/** Where the module of a custom form comes from: the same origin, so the CSP allows it. */
export const formModuleUrl = (resource: string) => `${base}/forms/${encodeURIComponent(resource)}`;
export const site = () => call<Site>("GET", "/site");

export const embed = (name: string, search: string) => call<Embed>("GET", `/embeds/${encodeURIComponent(name)}${search}`);

const listPath = (resource: string, scope?: Scope) =>
  scope ? `${encodeURIComponent(scope.resource)}/${scope.key}/${encodeURIComponent(scope.rel)}` : encodeURIComponent(resource);

/** `search` is the query string of the list as the URL holds it: `?q=..&f.x=..&n.x=..&sort=..&cursor=..&limit=..`. */
export const page = (resource: string, search: string, scope?: Scope) => call<Page>("GET", `/resources/${listPath(resource, scope)}${search}`);

/** The count is its own request, after the rows: capped at 10,000, or exact on request. It takes the search and the filters, not the position. */
export function count(resource: string, search: string, exact: boolean, scope?: Scope) {
  const from = new URLSearchParams(search);
  const to = new URLSearchParams();
  for (const [k, v] of from) if (k === "q" || k === "without" || k.startsWith("f.") || k.startsWith("n.")) to.append(k, v);
  if (exact) to.set("exact", "1");
  const text = to.toString();
  return call<Count>("GET", `/counts/${listPath(resource, scope)}${text ? `?${text}` : ""}`);
}

/** The first and last value of the date field of the list, or the rows that each period holds when `edges` are given (one count per two edges). */
export interface Dates {
  min?: string | null;
  max?: string | null;
  counts?: number[];
}

/** The edges go in the query as one list; `encodeURIComponent` keeps the `+` of an offset. */
export function dates(resource: string, search: string, edges?: string[]) {
  const from = new URLSearchParams(search);
  const keep = [...from].filter(([k]) => k === "q" || k === "without" || k.startsWith("f.") || k.startsWith("n."));
  const text = keep.map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(v)}`);
  if (edges) text.push(`edges=${encodeURIComponent(edges.join(","))}`);
  return call<Dates>("GET", `/dates/${encodeURIComponent(resource)}${text.length ? `?${text.join("&")}` : ""}`);
}

/** `without` is the read defaults to switch off, as the URL holds them (`a,b`). */
export const detail = (resource: string, key: string, without = "") =>
  call<Detail>("GET", `/resources/${encodeURIComponent(resource)}/${key}${without ? `?without=${encodeURIComponent(without)}` : ""}`);

const recordPath = (resource: string, key: string) => `/resources/${encodeURIComponent(resource)}/${key}`;

export type Values = Record<string, unknown>;

/** The records that a many-to-many save adds and removes, for each relation. A value is the key of the target record. */
export type Links = Record<string, { add: unknown[]; remove: unknown[] }>;

/** One row of a picker: the key of the target record and the text that names it. */
export interface Option {
  value: unknown;
  label: string;
}

/** Create a record from the fields that the user filled in, and the links of its chips. The answer has the encoded key of the new record. */
export const create = (resource: string, set: Values, links?: Links, inputs?: Values, inlines?: InlineChanges) =>
  call<{ key: string }>("POST", `/resources/${encodeURIComponent(resource)}`, { set, ...(links ? { links } : {}), ...(inputs ? { inputs } : {}), ...(inlines ? { inlines } : {}) });

/** Change a record: `set` holds only the changed fields, `loaded` the values that the form was loaded with. */
export const update = (resource: string, key: string, set: Values, loaded: Values, links?: Links, inputs?: Values, inlines?: InlineChanges) =>
  call<{ key: string; changed: string[] }>("PATCH", recordPath(resource, key), { set, loaded, ...(links ? { links } : {}), ...(inputs ? { inputs } : {}), ...(inlines ? { inlines } : {}) });

export const destroy = (resource: string, key: string, loaded: Values, inputs?: Values) =>
  call<{ key: string }>("DELETE", recordPath(resource, key), inputs ? { loaded, inputs } : { loaded });

/** The problems of one list save, by the index of the row in the request. */
export interface ListProblems {
  rows: Record<string, { fields?: Record<string, string[]>; form?: string[] }>;
  conflicts: Record<string, Record<string, { loaded: unknown; current: unknown }>>;
}

/** The problems that an error of a list save carries: a 400 names the rows, a 409 the changed fields. */
export function listProblems(e: unknown): ListProblems {
  const body = (e instanceof ApiError ? e.body : {}) as { rows?: ListProblems["rows"]; conflicts?: ListProblems["conflicts"] };
  return { rows: body.rows ?? {}, conflicts: body.conflicts ?? {} };
}

/** One request saves the edited cells of many rows of a list. `key` is the encoded key of the row. */
export const saveRows = (resource: string, rows: { key: string; set: Values; loaded: Values }[]) =>
  call<{ keys: string[]; changed: Record<string, string[]> }>("PATCH", `/resources/${encodeURIComponent(resource)}`, { rows });

const rowPath = (resource: string, key: string, inline: string) => `${recordPath(resource, key)}/inlines/${encodeURIComponent(inline)}`;

/** Row mode: each call writes one row of an inline alone, through the page of the child. */
export const rowCreate = (resource: string, key: string, inline: string, set: Values) => call<{ key: string }>("POST", rowPath(resource, key, inline), { set });
export const rowUpdate = (resource: string, key: string, inline: string, row: string, set: Values, loaded: Values) =>
  call<{ key: string; changed: string[] }>("PATCH", `${rowPath(resource, key, inline)}/${row}`, { set, loaded });
export const rowDestroy = (resource: string, key: string, inline: string, row: string, loaded: Values) => call<{ key: string }>("DELETE", `${rowPath(resource, key, inline)}/${row}`, { loaded });

/** One page of the picker of a relation field, by keyset: `next` is the cursor of the following page. */
export function autocomplete(resource: string, rel: string, q: string, cursor?: string) {
  const p = new URLSearchParams({ limit: "20" });
  if (q !== "") p.set("q", q);
  if (cursor) p.set("cursor", cursor);
  return call<{ items: Option[]; next: string | null }>("GET", `/autocomplete/${encodeURIComponent(resource)}/${encodeURIComponent(rel)}?${p}`);
}

/** The records that the record `key` links to through a many-to-many relation (at most 200). */
export const links = (resource: string, key: string, rel: string) =>
  call<{ items: Option[]; truncated: boolean }>("GET", `/links/${encodeURIComponent(resource)}/${key}/${encodeURIComponent(rel)}`);

/** Where the download of a file field comes from. The server reads the file from its storage and sends it as an attachment. */
export const fileUrl = (resource: string, key: string, field: string) => `${base}/files/${encodeURIComponent(resource)}/${key}/${encodeURIComponent(field)}`;

/** Store a file in a file field of a saved record. The upload writes the field of the record: `value` is the new reference. */
export function upload(resource: string, key: string, field: string, file: File) {
  const form = new FormData();
  form.append("file", file);
  return call<{ key: string; value: unknown }>("POST", `/files/${encodeURIComponent(resource)}/${key}/${encodeURIComponent(field)}`, form);
}

/** The rows that a read-only relation of the record `key` leads to (at most 200), each with the resource that holds it. */
export const linked = (resource: string, key: string, rel: string) =>
  call<{ items: (Option & { resource: string })[]; truncated: boolean }>("GET", `/links/${encodeURIComponent(resource)}/${key}/${encodeURIComponent(rel)}`);
