// The admin protocol, and nothing else: the SPA reads no other server path.

export interface Column {
  label: string;
  title: string;
  type: string;
  nullable: boolean;
  array: boolean;
  widget: string;
  choices?: string[];
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
}

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

export interface Page {
  resource: string;
  columns: Column[];
  rows: unknown[][];
  keys: string[];
  next: string | null;
  prev: string | null;
}

/** Where a list is: a keyset position or a page number, with the sort, the search and the page size. */
export interface ListParams {
  cursor?: string;
  before?: string;
  last?: boolean;
  page?: number;
  sort?: string;
  q?: string;
  limit?: number;
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
  links: { name: string; title: string; resource: string; key: string }[];
}

export interface User {
  key: unknown;
  username?: string;
  display?: string;
}

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly kind: string,
    message: string,
  ) {
    super(message);
  }
}

const base = document.querySelector<HTMLMetaElement>('meta[name="monorm-admin-api"]')?.content ?? "/admin/api";
let csrf = "";

async function call<T>(method: string, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = {};
  if (body !== undefined) headers["content-type"] = "application/json";
  if (method !== "GET") headers["x-csrf-token"] = csrf;
  const response = await fetch(`${base}${path}`, {
    method,
    headers,
    credentials: "same-origin",
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  let data: { ok?: boolean; error?: { kind: string; message: string }; csrf?: string } = {};
  try {
    data = text ? JSON.parse(text) : {};
  } catch {
    throw new ApiError(response.status, "BadResponse", `the server answered ${response.status} with no JSON`);
  }
  if (!response.ok || data.ok === false) {
    throw new ApiError(response.status, data.error?.kind ?? "Error", data.error?.message ?? `the server answered ${response.status}`);
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
export const site = () => call<Site>("GET", "/resources");

function query(params: Record<string, string | number | boolean | undefined>): string {
  const parts = Object.entries(params)
    .filter(([, v]) => v !== undefined && v !== "" && v !== false)
    .map(([k, v]) => `${k}=${encodeURIComponent(v === true ? "1" : String(v))}`);
  return parts.length ? `?${parts.join("&")}` : "";
}

export const page = (resource: string, params: ListParams) => call<Page>("GET", `/resources/${encodeURIComponent(resource)}${query({ ...params })}`);

/** The count is its own request, after the rows: capped at 10,000, or exact on request. */
export const count = (resource: string, q: string, exact: boolean) =>
  call<Count>("GET", `/counts/${encodeURIComponent(resource)}${query({ q, exact })}`);

/** `key` is the encoded path segment as the list gave it: it goes into the URL as it is. */
export const detail = (resource: string, key: string) => call<Detail>("GET", `/resources/${encodeURIComponent(resource)}/${key}`);
