/**
 * TypeScript ORM with a Rust core: the same schema, engine and query semantics as the
 * Python package.
 *
 * Models come from a schema file (`schema.prisma`) through a generated module
 * (`monorm generate typescript`), which types everything:
 *
 * ```ts
 * import { connect } from "monorm";
 * import { User } from "./models.js";
 *
 * await connect("postgres://...");
 * const users = await User.objects.filter(User.posts.createdAt.lt(yesterday));
 * ```
 */

export { Decimal } from "./decimal.js";
export type { JsonValue, In, ModelSpec, Hop, HopKind, ColType, FieldMeta, RelationMeta } from "./meta.js";
export { camel } from "./meta.js";
export {
  Column,
  Condition,
  Expression,
  Case,
  JsonPath,
  type TsQuery,
  type TsVector,
  Func,
  Ordering,
  ParamRef,
  RelationPath,
  ScalarSubquery,
  Window,
  WindowDef,
  WindowFunc,
  and,
  excluded,
  exists,
  func,
  not,
  or,
  outer,
  param,
  window,
} from "./expr.js";
export type { AggregateOptions, CaseDefault, Many, Compat, Operand, OrderOptions } from "./expr.js";
export { IndexRef, ModelMeta, Registry, column, define, describe, load, loads, registry } from "./model.js";
export type { FieldInfo, ModelInfo, RelationInfo } from "./model.js";
export type { Instance, ModelClass, Row, SchemaIR } from "./model.js";
export { BoundQuerySet, of } from "./bound.js";
export { InspectedStatement, InsertMany, InsertOne, ManyRelatedSet, OneRowQuery, Prepared, PreparedOneRowQuery, QuerySet, RelatedSet, UpsertOne, useModelMethods, useQuerySet } from "./query.js";
export type { Page, PageOptions } from "./pagination.js";
export type { AddOptions, BatchOptions, CopyOptions, FieldOrder, LoadAll, LoadItem, LoadOf, LockOptions, OnConflictOptions, PreparedInsert, PreparedUpdate, QueriesOf, QuerySetOf, Simplify, Via } from "./query.js";
export { Select } from "./select.js";
export { Cte, CteColumn } from "./cte.js";
export type { CteSelf } from "./cte.js";
export { Database, connect, getDatabase, scope } from "./db.js";
export type { ConnectOptions, QueryEvent, SessionLockOptions } from "./db.js";
export {
  DatabaseError,
  DecodeError,
  DoesNotExist,
  IntegrityError,
  LockNotAvailable,
  MultipleObjectsReturned,
  NotConnected,
  NotLoaded,
  ORMError,
  QueryError,
  SchemaError,
  TransactionAborted,
  TransactionRequired,
  VersionConflict,
  WriteProtected,
} from "./errors.js";
export { AdapterWriteError, FieldAdapterError, Pending, PreparedWrite, registerFieldAdapter } from "./adapters.js";
export type { FieldAdapter } from "./adapters.js";
export { allowWrites } from "./protection.js";
export * as debug from "./debug.js";
export { Migration, MigrationError, Migrations, Migrator, Pulled, pull } from "./migrations.js";
export type { Drift, Plan, Status, Step, SchemaSource } from "./migrations.js";

export type { QueryInspection, StatementInspection } from "./inspection.js";
export { statementInspection } from "./inspection.js";
