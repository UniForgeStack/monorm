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

export function page(resource: string, cursor: string | null): Promise<Page> {
  const query = cursor ? `?cursor=${encodeURIComponent(cursor)}` : "";
  return call<Page>("GET", `/resources/${encodeURIComponent(resource)}${query}`);
}

/** `key` is the encoded path segment as the list gave it: it goes into the URL as it is. */
export const detail = (resource: string, key: string) => call<Detail>("GET", `/resources/${encodeURIComponent(resource)}/${key}`);
