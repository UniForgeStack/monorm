// Extension packages: widgets and form modules that a package ships as static files.
// The contract (version 1, the admin major version): a module exports `adminMajor` (a number) and a default React component.
// A widget gets `WidgetProps`; a form module gets `FormProps` (see Form.tsx). The module may `import "react"`: the import map of the page gives it the React of the admin.
import { useEffect } from "react";
import { useQuery } from "@tanstack/react-query";
import * as api from "./api";
import type { ComponentType } from "react";

/** What a widget gets. The widget shows `value`, reports a change with `onChange` (any JSON value) and a problem with `onError`. */
export interface WidgetProps {
  id: string;
  name: string;
  /** The value of the field as the server sent it (`null` on a new record). */
  value: unknown;
  /** Set the value that the form sends. The form sends it only when it differs from the loaded value. */
  onChange: (value: unknown) => void;
  /** A message blocks the save and shows with the field. `null` clears it. */
  onError: (message: string | null) => void;
  field: { name: string; title: string; type: string; array: boolean; required: boolean; nullable: boolean; choices: string[]; max_length: number | null };
  /** The loaded value of each field of the form (`null` on a new record). */
  record: Record<string, unknown>;
  readOnly: boolean;
  disabled: boolean;
  /** The messages of the server for this field. */
  errors: string[];
  /** A custom route: the JSON answer, or a thrown error for a status of 400 or more. `path` is the part after `routes/`. */
  call: (method: string, path: string, body?: unknown) => Promise<unknown>;
  /** A custom route: the raw response, to read a stream. */
  stream: (method: string, path: string, body?: unknown) => Promise<Response>;
}

type Module = { default?: unknown; adminMajor?: unknown };

const styled = new Set<string>();

/** Adds the stylesheets of a package to the page, once. They load from this origin, so `style-src 'self'` allows them. */
function useStyles(pkg: Pick<api.PackageInfo, "styles"> | undefined) {
  useEffect(() => {
    for (const path of pkg?.styles ?? []) {
      if (styled.has(path)) continue;
      styled.add(path);
      const link = document.createElement("link");
      link.rel = "stylesheet";
      link.href = api.packageUrl(path);
      document.head.appendChild(link);
    }
  }, [pkg]);
}

/** The message for a module that was built for another admin major version, or null when it fits. */
export function versionProblem(what: string, module: Module, admin: number): string | null {
  if (module.adminMajor === admin) return null;
  if (typeof module.adminMajor !== "number") return `${what} does not export adminMajor, so this admin (major ${admin}) cannot tell which contract it uses.`;
  return `${what} was built for admin major ${module.adminMajor}, and this admin is major ${admin}. Rebuild the package for major ${admin}.`;
}

/** Loads a package module once and checks its version. */
export function usePackageModule(what: string, path: string | undefined, admin: number) {
  return useQuery({
    queryKey: ["package-module", path],
    enabled: path !== undefined,
    retry: false,
    staleTime: Infinity,
    gcTime: Infinity,
    queryFn: async () => {
      const module = (await import(/* @vite-ignore */ api.packageUrl(path as string))) as Module;
      const problem = versionProblem(what, module, admin);
      if (problem) throw new Error(problem);
      if (typeof module.default !== "function") throw new Error(`${what} has no default export that is a component.`);
      return module.default as ComponentType<never>;
    },
  });
}

/** The package and the widget path of `<package>.<widget>`, or undefined for a built-in widget. */
export function packageWidget(site: api.Site | undefined, w: string): { pkg: api.PackageInfo; path: string } | undefined {
  const [name, item] = w.split(".", 2);
  const pkg = site?.packages?.find((p) => p.name === name);
  const path = pkg && item !== undefined ? pkg.widgets[item] : undefined;
  return pkg && path ? { pkg, path } : undefined;
}

export function PackageWidget({ site, w, ...props }: { site: api.Site; w: string } & WidgetProps) {
  const found = packageWidget(site, w);
  useStyles(found?.pkg);
  const loaded = usePackageModule(`The widget "${w}"`, found?.path, site.admin_major);
  if (!found) return <p className="msg msg-error">The widget "{w}" is not in any package.</p>;
  if (loaded.isError) return <p className="msg msg-error" role="alert">{loaded.error instanceof Error ? loaded.error.message : String(loaded.error)}</p>;
  if (!loaded.data) return <span className="muted">Loading…</span>;
  const Widget = loaded.data as ComponentType<WidgetProps>;
  return <Widget {...props} />;
}

/** What a detail tab gets: the record that the page shows and the two ways to call a custom route. */
export interface TabProps {
  /** An embeddable tab hides all writes and forms when this is true. */
  embedded: boolean;
  resource: string;
  /** The key of the record, URL-encoded as in the address. */
  id: string;
  /** The loaded field labels and values of the record. */
  record: Record<string, unknown>;
  call: WidgetProps["call"];
  stream: WidgetProps["stream"];
}

/** A detail tab from a package: a component on the detail page. It reads its data from a custom route with `call`. No iframe is involved. */
export function PackageTab({ site, name, ...props }: { site: Pick<api.Site, "admin_major"> & { packages: Pick<api.PackageInfo, "name" | "tabs" | "styles">[] }; name: string } & TabProps) {
  const [pkgName, item] = name.split(".", 2);
  const pkg = site.packages?.find((p) => p.name === pkgName);
  const path = pkg && item !== undefined ? pkg.tabs[item] : undefined;
  useStyles(pkg);
  const loaded = usePackageModule(`The tab "${name}"`, path, site.admin_major);
  if (!path) return <p className="msg msg-error">The tab "{name}" is not in any package.</p>;
  if (loaded.isError) return <p className="msg msg-error" role="alert">{loaded.error instanceof Error ? loaded.error.message : String(loaded.error)}</p>;
  if (!loaded.data) return <span className="muted">Loading…</span>;
  const Tab = loaded.data as ComponentType<TabProps>;
  return <Tab {...props} />;
}

export const widgetCall = api.routeJson;
export const widgetStream = api.routeFetch;
