/** On-demand planner structure, independent of execution observation. */
import { getDatabase, type Database } from "./db.js";
import { NotConnected } from "./errors.js";
import type { ModelMeta } from "./model.js";
import type { NativeInspector } from "./native.js";

export interface StatementInspection {
  readonly hasBatching: boolean;
  readonly joinsCount: number;
  readonly correlatedCount: number;
  readonly uncorrelatedCount: number;
  readonly cteCount: number;
}
/** Planned templates, including prefetches. Empty parents can skip templates and
 * key batches can repeat them. Use Database.onQuery() for actual execution counts
 * and explain() for physical database strategy: correlation describes dependency.
 */
export interface QueryInspection {
  readonly statementCount: number;
  readonly hasBatching: boolean;
  readonly statements: readonly StatementInspection[];
}
/** @internal Match execution capabilities when this model has a connection. */
export function inspector(meta: ModelMeta, using?: Database): NativeInspector {
  if (using) return using.engine;
  try {
    const db = getDatabase();
    if (db.registry.has(meta.name) && db.registry.get(meta.name).model === meta.model) return db.engine;
  } catch (error) {
    if (!(error instanceof NotConnected)) throw error;
  }
  return meta.registry.native();
}
/** @internal Convert the shared Rust result to the immutable TypeScript API. */
export function inspection(payload: string): QueryInspection {
  const data = JSON.parse(payload) as {
    statement_count: number; has_batching: boolean;
    statements: { has_batching: boolean; joins_count: number;
      correlated_count: number; uncorrelated_count: number; cte_count: number }[];
  };
  return Object.freeze({
    statementCount: data.statement_count,
    hasBatching: data.has_batching,
    statements: Object.freeze(data.statements.map(s => Object.freeze({
      hasBatching: s.has_batching,
      joinsCount: s.joins_count,
      correlatedCount: s.correlated_count,
      uncorrelatedCount: s.uncorrelated_count,
      cteCount: s.cte_count,
    }))),
  });
}
/** @internal */
export function emptyInspection(): QueryInspection {
  return inspection('{"statement_count":0,"has_batching":false,"statements":[]}');
}
