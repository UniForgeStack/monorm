// Generated from schema.prisma by `orm generate typescript`. Do not edit.
//
// The models are built at runtime from the compiled schema below; the declarations
// give them their static types. Per model: the row type (`User`), the shapes insert /
// update / updateMany take, the columns and relation paths, and the model object.

/* eslint-disable */
import { define, type Column, type Compat, type Expression, type Hop, type In, type Instance, type JsonValue, type Many, type ManyRelatedSet, type ModelClass, type QuerySetOf, type RelatedSet, type RelationPath, type SchemaIR, type Via } from "../../src/index.js";

const SCHEMA: SchemaIR = {"models":[{"name":"Shop","table":"shop","fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"address_city","column":"address_city","type":"string","nullable":true,"nested":{"type":"Address","storage":"columns","field":"address","path":["city"],"under_null":true,"present_with":["address_floor","address_big","address_opened","address_geo_lat","address_geo_lng"]}},{"name":"address_floor","column":"address_floor","type":"int","nullable":true,"default":7,"nested":{"type":"Address","storage":"columns","field":"address","path":["floor"],"under_null":true,"present_with":["address_city","address_big","address_opened","address_geo_lat","address_geo_lng"]}},{"name":"address_big","column":"address_big","type":"big_int","nullable":true,"nested":{"type":"Address","storage":"columns","field":"address","path":["big"],"under_null":true,"present_with":["address_city","address_floor","address_opened","address_geo_lat","address_geo_lng"]}},{"name":"address_opened","column":"address_opened","type":"date_time","nullable":true,"nested":{"type":"Address","storage":"columns","field":"address","path":["opened"],"under_null":true,"present_with":["address_city","address_floor","address_big","address_geo_lat","address_geo_lng"]}},{"name":"address_geo_lat","column":"address_geo_lat","type":"float","nullable":true,"nested":{"type":"Address","storage":"columns","field":"address","path":["geo","lat"],"under_null":true,"present_with":["address_geo_lng"]}},{"name":"address_geo_lng","column":"address_geo_lng","type":"float","nullable":true,"nested":{"type":"Address","storage":"columns","field":"address","path":["geo","lng"],"under_null":true,"present_with":["address_geo_lat"]}},{"name":"snapshot","column":"snapshot","type":"json","nested":{"type":"Address","storage":"json","field":"snapshot"}}],"constraints":[{"kind":"check","expr":"(\"address_geo_lat\" IS NULL AND \"address_geo_lng\" IS NULL) OR (\"address_geo_lat\" IS NOT NULL)"},{"kind":"check","expr":"(\"address_city\" IS NULL AND \"address_floor\" IS NULL AND \"address_big\" IS NULL AND \"address_opened\" IS NULL AND \"address_geo_lat\" IS NULL AND \"address_geo_lng\" IS NULL) OR (\"address_city\" IS NOT NULL AND \"address_floor\" IS NOT NULL AND \"address_big\" IS NOT NULL AND \"address_opened\" IS NOT NULL)"}]}],"types":[{"name":"Geo","storage":"columns","fields":[{"name":"lat","nullable":false,"leaf":{"name":"lat","column":"lat","type":"float"}},{"name":"lng","nullable":true,"leaf":{"name":"lng","column":"lng","type":"float","nullable":true}}]},{"name":"Address","storage":"columns","fields":[{"name":"city","nullable":false,"leaf":{"name":"city","column":"city","type":"string"}},{"name":"floor","nullable":false,"leaf":{"name":"floor","column":"floor","type":"int","default":7}},{"name":"big","nullable":false,"leaf":{"name":"big","column":"big","type":"big_int"}},{"name":"opened","nullable":false,"leaf":{"name":"opened","column":"opened","type":"date_time"}},{"name":"geo","nullable":true,"type":"Geo"}]}]};

const models = define(SCHEMA, { requiredCapabilities: ["reference-loading"] });

const nested = (models as unknown as { readonly $types: Record<string, unknown> }).$types;

// -- Geo -------------------------------------------------------------------------------

/** A `Geo` value: immutable, compared with `equals()`. */
export interface Geo {
  readonly lat: number;
  readonly lng: number | null;
  equals(other: unknown): boolean;
  toJSON(): Record<string, unknown>;
}

export interface GeoInput {
  readonly lat: In<number>;
  readonly lng?: In<number> | null;
}

export interface GeoPatch {
  readonly lat?: In<number>;
  readonly lng?: In<number> | null;
}

export interface GeoClass {
  new (values: GeoInput): Geo;
}

/** The columns of a `Geo` member: the whole value, and one column per subfield. */
export type GeoRef<S extends string, H extends readonly Hop[], O extends boolean, N extends string, X> = Column<O extends true ? X | null : X, S, N, H> & {
  readonly lat: Column<O extends true ? number | null : null extends X ? number | null : number, S, "lat", H>;
  readonly lng: Column<O extends true ? number | null | null : null extends X ? number | null | null : number | null, S, "lng", H>;
};

export const Geo = nested["Geo"] as unknown as GeoClass;

// -- Address ---------------------------------------------------------------------------

/** A `Address` value: immutable, compared with `equals()`. */
export interface Address {
  readonly city: string;
  readonly floor: number;
  readonly big: bigint;
  readonly opened: Date;
  readonly geo: Geo | null;
  equals(other: unknown): boolean;
  toJSON(): Record<string, unknown>;
}

export interface AddressInput {
  readonly city: In<string>;
  readonly floor?: In<number>;
  readonly big: In<bigint>;
  readonly opened: In<Date>;
  readonly geo?: Geo | GeoInput | null;
}

export interface AddressPatch {
  readonly city?: In<string>;
  readonly floor?: In<number>;
  readonly big?: In<bigint>;
  readonly opened?: In<Date>;
  readonly geo?: Geo | GeoPatch | null;
}

export interface AddressClass {
  new (values: AddressInput): Address;
}

/** The columns of a `Address` member: the whole value, and one column per subfield. */
export type AddressRef<S extends string, H extends readonly Hop[], O extends boolean, N extends string, X> = Column<O extends true ? X | null : X, S, N, H> & {
  readonly city: Column<O extends true ? string | null : null extends X ? string | null : string, S, "city", H>;
  readonly floor: Column<O extends true ? number | null : null extends X ? number | null : number, S, "floor", H>;
  readonly big: Column<O extends true ? bigint | null : null extends X ? bigint | null : bigint, S, "big", H>;
  readonly opened: Column<O extends true ? Date | null : null extends X ? Date | null : Date, S, "opened", H>;
  readonly geo: GeoRef<S, H, O, "geo", Geo | null | (null extends X ? null : never)>;
};

export const Address = nested["Address"] as unknown as AddressClass;

// -- Shop ------------------------------------------------------------------------------

/** The column values of a Shop row. */
export interface ShopData {
  readonly id: number;
  readonly address: Address | null;
  readonly snapshot: Address;
}

/** A Shop row. To-one relations are typed on rows of queries that load them. */
export interface Shop extends ShopData, Instance<ShopSpec> {
}

export type ShopInsert = {
  id: In<number>;
  address?: Address | AddressInput | null;
  snapshot: Address | AddressInput;
};

export interface ShopUpdate {
  id?: In<number> | Expression<Compat<number>, "Shop" | "~Shop", {}>;
  address?: Address | AddressPatch | null;
  snapshot?: Address | AddressPatch;
}

export interface ShopUpdateRow {
  id: In<number>;
  address?: Address | AddressInput | null;
  snapshot?: Address | AddressInput;
}

export interface ShopSpec {
  readonly name: "Shop";
  readonly row: Shop;
  readonly data: ShopData;
  readonly insert: ShopInsert;
  readonly update: ShopUpdate;
  readonly updateRow: ShopUpdateRow;
  readonly pk: number;
}

export interface ShopFields<S extends string, H extends readonly Hop[], O extends boolean> {
  readonly id: Column<O extends true ? number | null : number, S, "id", H>;
  readonly address: AddressRef<S, H, O, "address", Address | null>;
  readonly snapshot: AddressRef<S, H, O, "snapshot", Address>;
}

export interface ShopPath<S extends string, H extends readonly Hop[], O extends boolean>
  extends RelationPath<ShopSpec, S, H>,
    ShopFields<S, H, O> {
  /** The relation query set for `load()`: `Shop.objects` bound to this relation. */
  readonly objects: QuerySetOf<ShopSpec, Shop, "Shop", {}, never, Via<H, undefined, false, S>>;
}

export interface ShopModel extends ModelClass<ShopSpec>, ShopFields<"Shop", [], false> {}

export const Shop = models["Shop"] as unknown as ShopModel;

