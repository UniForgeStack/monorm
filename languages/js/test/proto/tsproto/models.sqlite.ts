// Generated from schema.sqlite.prisma by `monorm generate typescript`. Do not edit.
//
// The models are built at runtime from the compiled schema below; the declarations
// give them their static types. Per model: the row type (`User`), the shapes insert /
// update / updateMany take, the columns and relation paths, and the model object.

/* eslint-disable */
import { define, type Column, type Compat, type Expression, type Hop, type In, type Instance, type Many, type ManyRelatedSet, type ModelClass, type QuerySetOf, type RelatedSet, type RelationPath, type SchemaIR, type Via } from "../../../src/index.js";

const SCHEMA: SchemaIR = {"dialect":"sqlite","models":[{"name":"TsProtoEvent","table":"ts_proto_event","mapped":true,"fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"body","column":"body","type":"bytes","nullable":true}]}],"behavior":{"read_defaults":[],"schema_contract":5,"schema_contract_minor":3,"declarations":[{"attribute":"proto.message","model":"TsProtoEvent","field":"body","field_type":"Bytes?","arguments":{},"positional":["acme.Note"],"location":{"file":"schema.sqlite.prisma","line":7,"column":15},"lowered":true}],"packages":{"monorm.proto":{"version":"0.1.4","manifest_hash":"9b77d9c30441e05678db8036d1380cdd07df12fb20ee9df6f396e45a51743965","passes":["monorm.proto.fields"],"attributes":["proto.message"],"languages":["python","typescript"],"databases":["postgres","sqlite"],"language_members":{"python":{"source":"integration/python/src/monorm_proto","query_set":"","instance":"","bound_query_set":"","overrides":[],"field_adapters":[{"adapter":"proto.message.v1","install":"install","module":"monorm_proto.model","imports":[],"value":"{class}","input":"{class}","members":[],"methods":[],"module_functions":[],"resolve":{"option":"message","setting":"proto"}}]},"typescript":{"source":"integration/typescript/src","query_set":"","instance":"","bound_query_set":"","overrides":[],"field_adapters":[{"adapter":"proto.message.v1","install":"install","module":"monorm/extensions/proto/model","imports":[],"value":"{class}","input":"{class}","members":[],"methods":[],"module_functions":[],"resolve":{"option":"message","setting":"proto"}}],"exports":{"./model":"model.js"}}},"dependencies":{}}},"lowered_models":["TsProtoEvent"],"result_fields":[],"storage":{"models":[{"name":"TsProtoEvent","table":"ts_proto_event","mapped":true,"fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"body","column":"body","type":"bytes","nullable":true}]}]},"field_storage":[],"owner_links":[],"field_adapters":[{"model":"TsProtoEvent","field":"body","adapter":"proto.message.v1","options":{"message":"acme.Note"},"null_only":true}]}};

const models = define(SCHEMA, { requiredCapabilities: ["reference-loading"] });

import * as _pm0 from "../gen-ts-proto/acme/note.js";
import { install as _fa0 } from "monorm/extensions/proto/model";
const _FA0 = [_fa0(models["TsProtoEvent"]!, new Map([["body", { options: {"message":"acme.Note", module: _pm0, symbol: "Note"}, nullable: true }]]))];

// -- TsProtoEvent ----------------------------------------------------------------------

/** The column values of a TsProtoEvent row. */
export interface TsProtoEventData {
  readonly id: number;
  readonly body: _pm0.Note | null;
}

/** A TsProtoEvent row. To-one relations are typed on rows of queries that load them. */
export interface TsProtoEvent extends TsProtoEventData, Instance<TsProtoEventSpec> {
}

export type TsProtoEventInsert = {
  id: In<number>;
  body?: _pm0.Note | null;
};

export interface TsProtoEventUpdate {
  id?: In<number> | Expression<Compat<number>, "TsProtoEvent" | "~TsProtoEvent", {}>;
  body?: _pm0.Note | null | Expression<Compat<_pm0.Note | null>, "TsProtoEvent" | "~TsProtoEvent", {}>;
}

export interface TsProtoEventUpdateRow {
  id: In<number>;
  body?: _pm0.Note | null;
}

export interface TsProtoEventSpec {
  readonly name: "TsProtoEvent";
  readonly row: TsProtoEvent;
  readonly data: TsProtoEventData;
  readonly insert: TsProtoEventInsert;
  readonly update: TsProtoEventUpdate;
  readonly updateRow: TsProtoEventUpdateRow;
  readonly pk: number;
}

export interface TsProtoEventFields<S extends string, H extends readonly Hop[], O extends boolean> {
  readonly id: Column<O extends true ? number | null : number, S, "id", H>;
  readonly body: Column<O extends true ? _pm0.Note | null | null : _pm0.Note | null, S, "body", H>;
}

export interface TsProtoEventPath<S extends string, H extends readonly Hop[], O extends boolean>
  extends RelationPath<TsProtoEventSpec, S, H>,
    TsProtoEventFields<S, H, O> {
  /** The relation query set for `load()`: `TsProtoEvent.objects` bound to this relation. */
  readonly objects: QuerySetOf<TsProtoEventSpec, TsProtoEvent, "TsProtoEvent", {}, never, Via<H, undefined, false, S>>;
}

export interface TsProtoEventModel extends ModelClass<TsProtoEventSpec>, TsProtoEventFields<"TsProtoEvent", [], false> {}

export const TsProtoEvent = models["TsProtoEvent"] as unknown as TsProtoEventModel;

