/** Keys for which an unfiltered eager reference load found no target row. */
import { key } from "./value-key.js";

const ABSENT = Symbol("orm.referenceAbsent");
type Marked = { [ABSENT]?: Map<string, readonly unknown[]> };
export function markAbsent(row: object, name: string, values: readonly unknown[]): void {
  if (values.some((v) => v === null || v === undefined)) return;
  const marked = row as Marked;
  marked[ABSENT] = new Map(marked[ABSENT]).set(name, values);
}
export function isAbsent(row: object, name: string, values: readonly unknown[]): boolean {
  const prior = (row as Marked)[ABSENT]?.get(name);
  return prior !== undefined && prior.length === values.length && prior.every((v, i) => key(v) === key(values[i]));
}
export function clearAbsent(row: object, name: string): void {
  const marked = row as Marked;
  if (marked[ABSENT]) {
    marked[ABSENT] = new Map(marked[ABSENT]);
    marked[ABSENT].delete(name);
  }
}
export function copyAbsent(row: object, fresh: object, name: string): void {
  clearAbsent(row, name);
  const values = (fresh as Marked)[ABSENT]?.get(name);
  if (values) markAbsent(row, name, values);
}

export function mergeAbsent(row: object, fresh: object): void {
  for (const [name, values] of (fresh as Marked)[ABSENT] ?? []) markAbsent(row, name, values);
}
