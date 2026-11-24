// Generated from schema.sqlite.prisma by `monorm generate typescript`. Do not edit.
//
// The models are built at runtime from the compiled schema below; the declarations
// give them their static types. Per model: the row type (`User`), the shapes insert /
// update / updateMany take, the columns and relation paths, and the model object.

/* eslint-disable */
import { define, type Column, type Compat, type Expression, type Hop, type In, type Instance, type Many, type ManyRelatedSet, type ModelClass, type QuerySetOf, type RelatedSet, type RelationPath, type SchemaIR, type Via } from "../../../src/index.js";

const SCHEMA: SchemaIR = {"dialect":"sqlite","models":[{"name":"CustomEvent","table":"proto_custom_event","mapped":true,"fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"plain","column":"plain","type":"bytes","nullable":true},{"name":"served","column":"served","type":"bytes","nullable":true}]}],"behavior":{"read_defaults":[],"schema_contract":5,"schema_contract_minor":3,"declarations":[{"attribute":"proto.message","model":"CustomEvent","field":"plain","field_type":"Bytes?","arguments":{},"positional":["fake.Plain"],"location":{"file":"schema.sqlite.prisma","line":7,"column":17},"lowered":true},{"attribute":"proto.message","model":"CustomEvent","field":"served","field_type":"Bytes?","arguments":{},"positional":["fake.Served"],"location":{"file":"schema.sqlite.prisma","line":8,"column":17},"lowered":true}],"packages":{"monorm.proto":{"version":"0.1.4","manifest_hash":"9b77d9c30441e05678db8036d1380cdd07df12fb20ee9df6f396e45a51743965","passes":["monorm.proto.fields"],"attributes":["proto.message"],"languages":["python","typescript"],"databases":["postgres","sqlite"],"language_members":{"python":{"source":"integration/python/src/monorm_proto","query_set":"","instance":"","bound_query_set":"","overrides":[],"field_adapters":[{"adapter":"proto.message.v1","install":"install","module":"monorm_proto.model","imports":[],"value":"{class}","input":"{class}","members":[],"methods":[],"module_functions":[],"resolve":{"option":"message","setting":"proto"}}]},"typescript":{"source":"integration/typescript/src","query_set":"","instance":"","bound_query_set":"","overrides":[],"field_adapters":[{"adapter":"proto.message.v1","install":"install","module":"monorm/extensions/proto/model","imports":[],"value":"{class}","input":"{class}","members":[],"methods":[],"module_functions":[],"resolve":{"option":"message","setting":"proto"}}],"exports":{"./model":"model.js"}}},"dependencies":{}}},"lowered_models":["CustomEvent"],"result_fields":[],"storage":{"models":[{"name":"CustomEvent","table":"proto_custom_event","mapped":true,"fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"plain","column":"plain","type":"bytes","nullable":true},{"name":"served","column":"served","type":"bytes","nullable":true}]}]},"field_storage":[],"owner_links":[],"field_adapters":[{"model":"CustomEvent","field":"plain","adapter":"proto.message.v1","options":{"message":"fake.Plain"},"null_only":true},{"model":"CustomEvent","field":"served","adapter":"proto.message.v1","options":{"message":"fake.Served"},"null_only":true}]}};

const models = define(SCHEMA, { requiredCapabilities: ["reference-loading"] });

import * as _pm0 from "./thing.js";
import { install as _fa0 } from "monorm/extensions/proto/model";
const _FA0 = [_fa0(models["CustomEvent"]!, new Map([["plain", { options: {"message":"fake.Plain", module: _pm0, symbol: "Plain"}, nullable: true }], ["served", { options: {"message":"fake.Served", module: _pm0, symbol: "Served"}, nullable: true }]]))];

// -- CustomEvent -----------------------------------------------------------------------

/** The column values of a CustomEvent row. */
export interface CustomEventData {
  readonly id: number;
  readonly plain: _pm0.Plain | null;
  readonly served: _pm0.Served | null;
}

/** A CustomEvent row. To-one relations are typed on rows of queries that load them. */
export interface CustomEvent extends CustomEventData, Instance<CustomEventSpec> {
}

export type CustomEventInsert = {
  id: In<number>;
  plain?: _pm0.Plain | null;
  served?: _pm0.Served | null;
};

export interface CustomEventUpdate {
  id?: In<number> | Expression<Compat<number>, "CustomEvent" | "~CustomEvent", {}>;
  plain?: _pm0.Plain | null | Expression<Compat<_pm0.Plain | null>, "CustomEvent" | "~CustomEvent", {}>;
  served?: _pm0.Served | null | Expression<Compat<_pm0.Served | null>, "CustomEvent" | "~CustomEvent", {}>;
}

export interface CustomEventUpdateRow {
  id: In<number>;
  plain?: _pm0.Plain | null;
  served?: _pm0.Served | null;
}

export interface CustomEventSpec {
  readonly name: "CustomEvent";
  readonly row: CustomEvent;
  readonly data: CustomEventData;
  readonly insert: CustomEventInsert;
  readonly update: CustomEventUpdate;
  readonly updateRow: CustomEventUpdateRow;
  readonly pk: number;
}

export interface CustomEventFields<S extends string, H extends readonly Hop[], O extends boolean> {
  readonly id: Column<O extends true ? number | null : number, S, "id", H>;
  readonly plain: Column<O extends true ? _pm0.Plain | null | null : _pm0.Plain | null, S, "plain", H>;
  readonly served: Column<O extends true ? _pm0.Served | null | null : _pm0.Served | null, S, "served", H>;
}

export interface CustomEventPath<S extends string, H extends readonly Hop[], O extends boolean>
  extends RelationPath<CustomEventSpec, S, H>,
    CustomEventFields<S, H, O> {
  /** The relation query set for `load()`: `CustomEvent.objects` bound to this relation. */
  readonly objects: QuerySetOf<CustomEventSpec, CustomEvent, "CustomEvent", {}, never, Via<H, undefined, false, S>>;
}

export interface CustomEventModel extends ModelClass<CustomEventSpec>, CustomEventFields<"CustomEvent", [], false> {}

export const CustomEvent = models["CustomEvent"] as unknown as CustomEventModel;

