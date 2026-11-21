/** Scoped bypass for model write rules. */

import { AsyncLocalStorage } from "node:async_hooks";

import type { ModelMeta } from "./model.js";

/** The model names the current scope allows to write; scopes nest by union. */
const scope = new AsyncLocalStorage<readonly string[]>();
const none: readonly string[] = [];

/**
 * Runs `fn` with the application write rules of `models` open. On PostgreSQL it also
 * opens skippable database rules for a database role listed in `bypass_roles`.
 * Work that `fn` starts shares the scope. The function starts no transaction.
 */
export function allowWrites<T>(models: readonly { readonly _meta: ModelMeta }[], fn: () => Promise<T>): Promise<T> {
  // A Promise API: wrong arguments reject, they do not throw.
  if (!Array.isArray(models) || typeof fn !== "function") {
    return Promise.reject(new TypeError("allowWrites() takes a list of models and a function: allowWrites([Post], async () => ...)"));
  }
  const names: string[] = [];
  for (const m of models as readonly unknown[]) {
    const meta = (typeof m === "object" || typeof m === "function") && m !== null ? (m as { _meta?: ModelMeta })._meta : undefined;
    if (!meta) return Promise.reject(new TypeError(`allowWrites() takes models, got ${String(m)}`));
    names.push(meta.name);
  }
  return scope.run([...allowedWrites(), ...names], async () => await fn());
}

/** @internal The model names the current scope allows to write. */
export function allowedWrites(): readonly string[] {
  return scope.getStore() ?? none;
}
