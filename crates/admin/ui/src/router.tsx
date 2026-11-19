import { useSyncExternalStore, type AnchorHTMLAttributes, type MouseEvent } from "react";

/** The base path of this admin: the server writes it into the index page, so no path is fixed in the build. */
export const BASE = document.querySelector<HTMLMetaElement>('meta[name="monorm-admin-base"]')?.content ?? "/admin";

const listeners = new Set<() => void>();
const subscribe = (fn: () => void) => {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
};
const here = () => window.location.pathname + window.location.search;
let shown = here();
/** The place of the shown entry in the stack: each entry that `navigate` makes holds its number in `history.state`. */
let index: number = window.history.state?.i ?? 0;
/** The `popstate` events that our own `history.go` will raise: they are no user move. */
let undo = 0;
window.history.replaceState({ i: index }, "", window.location.href);

/** The path of the page, as the browser holds it (still percent-encoded). */
export function usePath(): string {
  return useSyncExternalStore(subscribe, () => window.location.pathname);
}

/** The query string of the page, as the browser holds it: the state of a list lives here. */
export function useSearch(): string {
  return useSyncExternalStore(subscribe, () => window.location.search);
}

let blocker: (() => boolean) | null = null;

/** While a page holds unsaved work, `ask` runs before a page change and returns false to stay. One holder at a time. */
export function blockNavigation(ask: () => boolean): () => void {
  blocker = ask;
  return () => {
    if (blocker === ask) blocker = null;
  };
}

export function navigate(to: string, replace = false) {
  if (blocker && !blocker()) return;
  if (replace) window.history.replaceState({ i: index }, "", to);
  else window.history.pushState({ i: ++index }, "", to);
  shown = here();
  listeners.forEach((fn) => fn());
}

// Back and Forward ask too. The browser has moved already: `history.go` undoes the move, so a refusal keeps the whole stack.
// A move with no known distance (an entry that this code did not make) falls back to a new entry.
window.addEventListener("popstate", () => {
  if (undo > 0) {
    undo--;
    return;
  }
  const at: unknown = window.history.state?.i;
  const to = typeof at === "number" ? at : index;
  if (blocker && !blocker()) {
    if (to === index) {
      window.history.pushState({ i: ++index }, "", shown);
    } else {
      undo++;
      window.history.go(index - to);
    }
    return;
  }
  index = to;
  shown = here();
  listeners.forEach((fn) => fn());
});

export type Route =
  | { page: "login" }
  | { page: "password" }
  | { page: "home" }
  | { page: "list"; resource: string }
  | { page: "detail"; resource: string; key: string }
  | { page: "new"; resource: string }
  | { page: "edit"; resource: string; key: string };

/** `<base>/-/login`, `<base>/-/password`, `<base>/<resource>`, `<base>/<resource>/<key>`, `<base>/<resource>/<key>/edit` and `<base>/<resource>/-/new`: `-` is no resource name and no key. */
export function parse(pathname: string): Route {
  const rest = pathname.startsWith(BASE) ? pathname.slice(BASE.length) : pathname;
  const parts = rest.split("/").filter((p) => p !== "");
  const [first, second, third] = parts;
  if (first === undefined) return { page: "home" };
  if (first === "-") return second === "password" ? { page: "password" } : { page: "login" };
  const resource = decodeURIComponent(first);
  if (second === undefined) return { page: "list", resource };
  if (second === "-" && third === "new") return { page: "new", resource };
  if (third === "edit") return { page: "edit", resource, key: second };
  return { page: "detail", resource, key: second };
}

export const listUrl = (resource: string) => `${BASE}/${encodeURIComponent(resource)}`;
export const detailUrl = (resource: string, key: string) => `${listUrl(resource)}/${key}`;
export const newUrl = (resource: string, search = "") => `${listUrl(resource)}/-/new${search}`;
export const editUrl = (resource: string, key: string) => `${detailUrl(resource, key)}/edit`;
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

const LAST = "monorm-admin-list:";

/** The view of a list (its query string) lives for the tab: the detail page links back to it. */
export function rememberList(resource: string, search: string) {
  if (search) sessionStorage.setItem(LAST + resource, search);
  else sessionStorage.removeItem(LAST + resource);
}

/** The URL of a list as the user left it. */
export function listBack(resource: string): string {
  const search = sessionStorage.getItem(LAST + resource);
  return search ? `${listUrl(resource)}?${search}` : listUrl(resource);
}
