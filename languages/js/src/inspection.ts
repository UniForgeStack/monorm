/** On-demand planner structure, independent of execution observation. */
import { getDatabase, type Database } from "./db.js";
import { NotConnected } from "./errors.js";
import type { ModelMeta } from "./model.js";
import type { NativeInspector } from "./native.js";

/** A subquery has a shape: the way the outer query uses its result.
 * `scalar*` is a value.
 * `set*` is IN, EXISTS, ANY or ALL.
 * `derived*` is a FROM or JOIN item (LATERAL included).
 * Each shape counts `Correlated` and `Uncorrelated` subqueries.
 * A CTE counts in `cteCount`.
 */
export interface StatementInspection {
  readonly hasBatching: boolean;
  readonly joinsCount: number;
  readonly scalarCorrelated: number;
  readonly scalarUncorrelated: number;
  readonly setCorrelated: number;
  readonly setUncorrelated: number;
  readonly derivedCorrelated: number;
  readonly derivedUncorrelated: number;
  readonly cteCount: number;
}
const ZERO_STATEMENT: StatementInspection = Object.freeze({
  hasBatching: false,
  joinsCount: 0,
  scalarCorrelated: 0,
  scalarUncorrelated: 0,
  setCorrelated: 0,
  setUncorrelated: 0,
  derivedCorrelated: 0,
  derivedUncorrelated: 0,
  cteCount: 0,
});
/** Build an expected statement: omitted counts are zero. Use it in `deepStrictEqual`. */
export function statementInspection(fields: Partial<StatementInspection> = {}): StatementInspection {
  return Object.freeze({ ...ZERO_STATEMENT, ...fields });
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
      scalar_correlated: number; scalar_uncorrelated: number;
      set_correlated: number; set_uncorrelated: number;
      derived_correlated: number; derived_uncorrelated: number; cte_count: number }[];
  };
  return Object.freeze({
    statementCount: data.statement_count,
    hasBatching: data.has_batching,
    statements: Object.freeze(data.statements.map(s => Object.freeze({
      hasBatching: s.has_batching,
      joinsCount: s.joins_count,
      scalarCorrelated: s.scalar_correlated,
      scalarUncorrelated: s.scalar_uncorrelated,
      setCorrelated: s.set_correlated,
      setUncorrelated: s.set_uncorrelated,
      derivedCorrelated: s.derived_correlated,
      derivedUncorrelated: s.derived_uncorrelated,
      cteCount: s.cte_count,
    }))),
  });
}
/** @internal */
export function emptyInspection(): QueryInspection {
  return inspection('{"statement_count":0,"has_batching":false,"statements":[]}');
}
