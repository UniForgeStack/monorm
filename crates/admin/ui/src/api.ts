// The admin protocol, and nothing else: the SPA reads no other server path.

export interface Column {
  label: string;
  title: string;
  type: string;
  nullable: boolean;
  array: boolean;
  widget: string;
  choices?: string[];
  /** What an empty value shows, when the config sets a text. */
  empty?: string;
  /** The value is computed: it has no field of its own. */
  computed?: boolean;
}

export interface Filter {
  name: string;
  title: string;
  kind: "enum" | "bool" | "number" | "datetime" | "date" | "uuid" | "text" | "relation";
  nullable: boolean;
  operators: string[];
  choices?: string[];
  /** A relation filter: the resource whose key is the value. */
  target?: string;
}

export interface NamedFilter {
  name: string;
  title: string;
  default: string | null;
  choices: { name: string; title: string }[];
}

export interface Tab {
  name: string;
  title: string;
  target: string;
  /** "Add" on the tab: the field of the new record, and the field of the open record that gives its value. */
  prefill?: { parent: string; child: string }[];
}

/** A relation field of a form: a picker (one record) or chips (many records) over the resource `target`. */
export interface RelationInfo {
  name: string;
  target: string;
  /** The column of the target that names a row. */
  label: string;
  /** The user may create a target record from the form (a modal). */
  create: boolean;
  /** The config limits the choices. */
  choices: boolean;
}

/** What a form needs to know of one writable field. */
export interface FormField {
  name: string;
  /** Set when a picker fills the field. */
  relation?: RelationInfo | null;
  /** The field holds a file reference: the form uploads and downloads it. */
  file?: boolean;
  required: boolean;
  default: boolean;
  readonly_on_update: boolean;
  /** The config makes the field read-only on both forms. */
  readonly: boolean;
  /** The widget after the config: the field, the nested path, the type, then the default of the column. */
  widget: string;
  max_length: number | null;
}

/** A block of fields in a form. `title` is null for the block without a heading. */
export interface Section {
  title: string | null;
  fields: string[];
  collapsed: boolean;
}

export interface Resource {
  name: string;
  title: string;
  group: string | null;
  columns: Column[];
  fields: Column[];
  key: string[];
  links: { name: string; title: string; target: string }[];
  page_size: number;
  max_page_size: number;
  sortable: { column: string; indexed: boolean }[];
  search: { fields: string[]; custom: boolean };
  filters: Filter[];
  named_filters: NamedFilter[];
  tabs: Tab[];
  empty: string | null;
  /** The rules that a row can allow: `change`, `delete`. Each row and detail carries one flag for each. */
  can?: string[];
  /** The read defaults that this user may switch off, with `?without=<name>`. */
  bypass?: string[];
  /** False when the admin does not write to the model: `read_only_reason` says why. */
  writable?: boolean;
  read_only_reason?: string | null;
  form?: FormField[];
  /** The many-to-many relations that the form edits as chips. */
  chips?: RelationInfo[];
  /** The layout of the create form and of the edit form. */
  sections?: { create: Section[]; edit: Section[] };
  /** A module of the resource replaces the whole form: see `formModuleUrl`. */
  form_component?: boolean;
  /** The writes that this user may do: `create`, `update`, `delete`. */
  ops?: string[];
}

/** Whether the user may do the write. The server checks it again. */
export const may = (r: Resource | undefined, op: "create" | "update" | "delete") => !!r && r.writable !== false && !!r.form && (r.ops ?? [op]).includes(op);

export interface Brand {
  name: string;
  logo: string | null;
  colors: { primary?: string; accent?: string };
  links: { label: string; url: string }[];
  environment: string | null;
}

export interface Nav {
  group: string | null;
  resources: string[];
}

export interface Site {
  brand: Brand;
  nav: Nav[];
  resources: Resource[];
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

export interface Detail {
  resource: string;
  key: string;
  fields: Column[];
  values: unknown[];
  flags?: Flags;
  links: { name: string; title: string; resource: string; key: string }[];
}

export interface User {
  key: unknown;
  username?: string;
  display?: string;
}

/** The parts of an error answer that a form shows: a message for each field, messages for the form, and the 409 values. */
export interface ErrorBody {
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

const base = document.querySelector<HTMLMetaElement>('meta[name="monorm-admin-api"]')?.content ?? "/admin/api";
let csrf = "";

async function call<T>(method: string, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = {};
  // A `FormData` body is the upload: the browser sets the multipart content type with its boundary.
  const upload = body instanceof FormData;
  if (body !== undefined && !upload) headers["content-type"] = "application/json";
  if (method !== "GET") headers["x-csrf-token"] = csrf;
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
}

/** The login state: the token for writes, and the user when a session exists. */
export async function session(): Promise<Session> {
  const state = await call<{ authenticated: boolean }>("GET", "/login");
  if (!state.authenticated) return { authenticated: false, user: null };
  const me = await call<{ user: User }>("GET", "/me");
  return { authenticated: true, user: me.user };
}

export const login = (username: string, password: string) => call<{ user: User }>("POST", "/login", { username, password });
export const logout = () => call<object>("POST", "/logout", {});
export const changePassword = (old_password: string, new_password: string) => call<object>("POST", "/password", { old_password, new_password });
/** Where the module of a custom form comes from: the same origin, so the CSP allows it. */
export const formModuleUrl = (resource: string) => `${base}/forms/${encodeURIComponent(resource)}`;
export const site = () => call<Site>("GET", "/resources");

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
export const create = (resource: string, set: Values, links?: Links) =>
  call<{ key: string }>("POST", `/resources/${encodeURIComponent(resource)}`, links ? { set, links } : { set });

/** Change a record: `set` holds only the changed fields, `loaded` the values that the form was loaded with. */
export const update = (resource: string, key: string, set: Values, loaded: Values, links?: Links) =>
  call<{ key: string; changed: string[] }>("PATCH", recordPath(resource, key), links ? { set, loaded, links } : { set, loaded });

export const destroy = (resource: string, key: string, loaded: Values) => call<{ key: string }>("DELETE", recordPath(resource, key), { loaded });

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
