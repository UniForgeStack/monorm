// Generated from schema.prisma by `monorm generate typescript`. Do not edit.
//
// The models are built at runtime from the compiled schema below; the declarations
// give them their static types. Per model: the row type (`User`), the shapes insert /
// update / updateMany take, the columns and relation paths, and the model object.

/* eslint-disable */
import { define, type Column, type Compat, type Expression, type Hop, type In, type Instance, type Many, type ManyRelatedSet, type ModelClass, type QuerySetOf, type RelatedSet, type RelationPath, type SchemaIR, type Via } from "../../../src/index.js";

const SCHEMA: SchemaIR = {"models":[{"name":"ProtoEvent","table":"proto_event","mapped":true,"fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"payload","column":"payload","type":"bytes","nullable":true},{"name":"inner","column":"inner","type":"bytes","nullable":true},{"name":"bill","column":"bill","type":"bytes","nullable":true},{"name":"raw","column":"raw","type":"bytes","nullable":true}]}],"behavior":{"read_defaults":[],"schema_contract":5,"schema_contract_minor":3,"declarations":[{"attribute":"proto.message","model":"ProtoEvent","field":"payload","field_type":"Bytes?","arguments":{},"positional":["acme.Note"],"location":{"file":"schema.prisma","line":3,"column":18},"lowered":true},{"attribute":"proto.message","model":"ProtoEvent","field":"inner","field_type":"Bytes?","arguments":{},"positional":["acme.Outer.Inner"],"location":{"file":"schema.prisma","line":4,"column":18},"lowered":true},{"attribute":"proto.message","model":"ProtoEvent","field":"bill","field_type":"Bytes?","arguments":{},"positional":["acme.billing.Invoice"],"location":{"file":"schema.prisma","line":5,"column":18},"lowered":true}],"packages":{"monorm.proto":{"version":"0.1.2","manifest_hash":"38755e18fb79ff22eae8cdf6154291b58df6414664463f8cc123745a9ac9bdd7","passes":["monorm.proto.fields"],"attributes":["proto.message"],"languages":["python","typescript"],"databases":["postgres","sqlite"],"language_members":{"python":{"source":"integration/python/src/monorm_proto","query_set":"","instance":"","bound_query_set":"","overrides":[],"field_adapters":[{"adapter":"proto.message.v1","install":"install","module":"monorm_proto.model","imports":[],"value":"{class}","input":"{class}","members":[],"methods":[],"module_functions":[],"resolve":{"option":"message","setting":"proto"}}]},"typescript":{"source":"integration/typescript/src","query_set":"","instance":"","bound_query_set":"","overrides":[],"field_adapters":[{"adapter":"proto.message.v1","install":"install","module":"monorm/extensions/proto/model","imports":[],"value":"{class}","input":"{class}","members":[],"methods":[],"module_functions":[],"resolve":{"option":"message","setting":"proto"}}],"exports":{"./model":"model.js"}}},"dependencies":{}}},"lowered_models":["ProtoEvent"],"result_fields":[],"storage":{"models":[{"name":"ProtoEvent","table":"proto_event","mapped":true,"fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"payload","column":"payload","type":"bytes","nullable":true},{"name":"inner","column":"inner","type":"bytes","nullable":true},{"name":"bill","column":"bill","type":"bytes","nullable":true},{"name":"raw","column":"raw","type":"bytes","nullable":true}]}]},"field_storage":[],"owner_links":[],"field_adapters":[{"model":"ProtoEvent","field":"payload","adapter":"proto.message.v1","options":{"message":"acme.Note"},"null_only":true},{"model":"ProtoEvent","field":"inner","adapter":"proto.message.v1","options":{"message":"acme.Outer.Inner"},"null_only":true},{"model":"ProtoEvent","field":"bill","adapter":"proto.message.v1","options":{"message":"acme.billing.Invoice"},"null_only":true}]}};

const models = define(SCHEMA, { requiredCapabilities: ["reference-loading"] });

import * as _pm0 from "../gen/acme/note_pb.js";
import * as _pm1 from "../gen/acme/billing/invoice_pb.js";
import { install as _fa0 } from "monorm/extensions/proto/model";
const _FA0 = [_fa0(models["ProtoEvent"]!, new Map([["payload", { options: {"message":"acme.Note", module: _pm0, symbol: "Note"}, nullable: true }], ["inner", { options: {"message":"acme.Outer.Inner", module: _pm0, symbol: "Outer_Inner"}, nullable: true }], ["bill", { options: {"message":"acme.billing.Invoice", module: _pm1, symbol: "Invoice"}, nullable: true }]]))];

// -- ProtoEvent ------------------------------------------------------------------------

/** The column values of a ProtoEvent row. */
export interface ProtoEventData {
  readonly id: number;
  readonly payload: _pm0.Note | null;
  readonly inner: _pm0.Outer_Inner | null;
  readonly bill: _pm1.Invoice | null;
  readonly raw: Uint8Array | null;
}

/** A ProtoEvent row. To-one relations are typed on rows of queries that load them. */
export interface ProtoEvent extends ProtoEventData, Instance<ProtoEventSpec> {
}

export type ProtoEventInsert = {
  id: In<number>;
  payload?: _pm0.Note | null;
  inner?: _pm0.Outer_Inner | null;
  bill?: _pm1.Invoice | null;
  raw?: In<Uint8Array | null>;
};

export interface ProtoEventUpdate {
  id?: In<number> | Expression<Compat<number>, "ProtoEvent" | "~ProtoEvent", {}>;
  payload?: _pm0.Note | null | Expression<Compat<_pm0.Note | null>, "ProtoEvent" | "~ProtoEvent", {}>;
  inner?: _pm0.Outer_Inner | null | Expression<Compat<_pm0.Outer_Inner | null>, "ProtoEvent" | "~ProtoEvent", {}>;
  bill?: _pm1.Invoice | null | Expression<Compat<_pm1.Invoice | null>, "ProtoEvent" | "~ProtoEvent", {}>;
  raw?: In<Uint8Array | null> | Expression<Compat<Uint8Array | null>, "ProtoEvent" | "~ProtoEvent", {}>;
}

export interface ProtoEventUpdateRow {
  id: In<number>;
  payload?: _pm0.Note | null;
  inner?: _pm0.Outer_Inner | null;
  bill?: _pm1.Invoice | null;
  raw?: In<Uint8Array | null>;
}

export interface ProtoEventSpec {
  readonly name: "ProtoEvent";
  readonly row: ProtoEvent;
  readonly data: ProtoEventData;
  readonly insert: ProtoEventInsert;
  readonly update: ProtoEventUpdate;
  readonly updateRow: ProtoEventUpdateRow;
  readonly pk: number;
}

export interface ProtoEventFields<S extends string, H extends readonly Hop[], O extends boolean> {
  readonly id: Column<O extends true ? number | null : number, S, "id", H>;
  readonly payload: Column<O extends true ? _pm0.Note | null | null : _pm0.Note | null, S, "payload", H>;
  readonly inner: Column<O extends true ? _pm0.Outer_Inner | null | null : _pm0.Outer_Inner | null, S, "inner", H>;
  readonly bill: Column<O extends true ? _pm1.Invoice | null | null : _pm1.Invoice | null, S, "bill", H>;
  readonly raw: Column<O extends true ? Uint8Array | null | null : Uint8Array | null, S, "raw", H>;
}

export interface ProtoEventPath<S extends string, H extends readonly Hop[], O extends boolean>
  extends RelationPath<ProtoEventSpec, S, H>,
    ProtoEventFields<S, H, O> {
  /** The relation query set for `load()`: `ProtoEvent.objects` bound to this relation. */
  readonly objects: QuerySetOf<ProtoEventSpec, ProtoEvent, "ProtoEvent", {}, never, Via<H, undefined, false, S>>;
}

export interface ProtoEventModel extends ModelClass<ProtoEventSpec>, ProtoEventFields<"ProtoEvent", [], false> {}

export const ProtoEvent = models["ProtoEvent"] as unknown as ProtoEventModel;

