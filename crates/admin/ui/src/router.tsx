import { useSyncExternalStore, type AnchorHTMLAttributes, type MouseEvent } from "react";

export const BASE = "/admin";

const listeners = new Set<() => void>();
const subscribe = (fn: () => void) => {
  listeners.add(fn);
  window.addEventListener("popstate", fn);
  return () => {
    listeners.delete(fn);
    window.removeEventListener("popstate", fn);
  };
};

/** The path below /admin, as the browser holds it (still percent-encoded). */
export function usePath(): string {
  return useSyncExternalStore(subscribe, () => window.location.pathname);
}

export function navigate(to: string, replace = false) {
  if (replace) window.history.replaceState(null, "", to);
  else window.history.pushState(null, "", to);
  listeners.forEach((fn) => fn());
}

export type Route =
  | { page: "login" }
  | { page: "password" }
  | { page: "home" }
  | { page: "list"; resource: string }
  | { page: "detail"; resource: string; key: string };

/** `/admin/-/login`, `/admin/-/password`, `/admin/<resource>` and `/admin/<resource>/<key>`: `-` is no resource name. */
export function parse(pathname: string): Route {
  const rest = pathname.startsWith(BASE) ? pathname.slice(BASE.length) : pathname;
  const parts = rest.split("/").filter((p) => p !== "");
  const [first, second] = parts;
  if (first === undefined) return { page: "home" };
  if (first === "-") return second === "password" ? { page: "password" } : { page: "login" };
  const resource = decodeURIComponent(first);
  if (second === undefined) return { page: "list", resource };
  return { page: "detail", resource, key: second };
}

export const listUrl = (resource: string) => `${BASE}/${encodeURIComponent(resource)}`;
export const detailUrl = (resource: string, key: string) => `${listUrl(resource)}/${key}`;
export const LOGIN_URL = `${BASE}/-/login`;
export const PASSWORD_URL = `${BASE}/-/password`;

export function Link({ href, onClick, ...rest }: AnchorHTMLAttributes<HTMLAnchorElement> & { href: string }) {
  const go = (e: MouseEvent<HTMLAnchorElement>) => {
    onClick?.(e);
    if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey || rest.target === "_blank") return;
    if (!href.startsWith(BASE)) return;
    e.preventDefault();
    navigate(href);
  };
  return <a href={href} onClick={go} {...rest} />;
}
