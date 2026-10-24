// Generated from schema.prisma by `orm generate typescript`. Do not edit.
//
// The models are built at runtime from the compiled schema below; the declarations
// give them their static types. Per model: the row type (`User`), the shapes insert /
// update / updateMany take, the columns and relation paths, and the model object.

/* eslint-disable */
import { define, type Column, type Compat, type Expression, type Hop, type In, type IndexRef, type Instance, type JsonValue, type Many, type ManyRelatedSet, type ModelClass, type QuerySetOf, type RelatedSet, type RelationPath, type SchemaIR, type Via } from "orm";

const SCHEMA: SchemaIR = {"models":[{"name":"Shop","table":"shop","fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"address_city","column":"address_city","type":"string","nullable":true,"nested":{"type":"Address","storage":"columns","field":"address","path":["city"],"under_null":true,"present_with":["address_floor","address_big","address_opened","address_geo_lat","address_geo_lng"]}},{"name":"address_floor","column":"address_floor","type":"int","nullable":true,"default":7,"nested":{"type":"Address","storage":"columns","field":"address","path":["floor"],"under_null":true,"present_with":["address_city","address_big","address_opened","address_geo_lat","address_geo_lng"]}},{"name":"address_big","column":"address_big","type":"big_int","nullable":true,"nested":{"type":"Address","storage":"columns","field":"address","path":["big"],"under_null":true,"present_with":["address_city","address_floor","address_opened","address_geo_lat","address_geo_lng"]}},{"name":"address_opened","column":"address_opened","type":"date_time","nullable":true,"nested":{"type":"Address","storage":"columns","field":"address","path":["opened"],"under_null":true,"present_with":["address_city","address_floor","address_big","address_geo_lat","address_geo_lng"]}},{"name":"address_geo_lat","column":"address_geo_lat","type":"float","nullable":true,"nested":{"type":"Address","storage":"columns","field":"address","path":["geo","lat"],"under_null":true,"present_with":["address_geo_lng"]}},{"name":"address_geo_lng","column":"address_geo_lng","type":"float","nullable":true,"nested":{"type":"Address","storage":"columns","field":"address","path":["geo","lng"],"under_null":true,"present_with":["address_geo_lat"]}},{"name":"snapshot","column":"snapshot","type":"json","nested":{"type":"Address","storage":"json","field":"snapshot"}}],"constraints":[{"kind":"check","expr":"(\"address_geo_lat\" IS NULL AND \"address_geo_lng\" IS NULL) OR (\"address_geo_lat\" IS NOT NULL)"},{"kind":"check","expr":"(\"address_city\" IS NULL AND \"address_floor\" IS NULL AND \"address_big\" IS NULL AND \"address_opened\" IS NULL AND \"address_geo_lat\" IS NULL AND \"address_geo_lng\" IS NULL) OR (\"address_city\" IS NOT NULL AND \"address_floor\" IS NOT NULL AND \"address_big\" IS NOT NULL AND \"address_opened\" IS NOT NULL)"}]},{"name":"Plain","table":"plain","fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"g_lat","column":"g_lat","type":"float","nested":{"type":"Geo","storage":"columns","field":"g","path":["lat"],"root_required":true}},{"name":"g_lng","column":"g_lng","type":"float","nullable":true,"nested":{"type":"Geo","storage":"columns","field":"g","path":["lng"],"root_required":true}}]},{"name":"Wrapped","table":"wrapped","fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"w_inner_lat","column":"w_inner_lat","type":"float","nullable":true,"nested":{"type":"Wrap","storage":"columns","field":"w","path":["inner","lat"],"under_null":true,"root_required":true,"present_with":["w_inner_lng"]}},{"name":"w_inner_lng","column":"w_inner_lng","type":"float","nullable":true,"nested":{"type":"Wrap","storage":"columns","field":"w","path":["inner","lng"],"under_null":true,"root_required":true,"present_with":["w_inner_lat"]}}],"constraints":[{"kind":"check","expr":"(\"w_inner_lat\" IS NULL AND \"w_inner_lng\" IS NULL) OR (\"w_inner_lat\" IS NOT NULL)"}]},{"name":"Opt","table":"opt","fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"t_tag","column":"t_tag","type":"string","nullable":true,"nested":{"type":"Tagged","storage":"columns","field":"t","path":["tag"],"under_null":true,"present_with":["t_inner_lat","t_inner_lng"]}},{"name":"t_inner_lat","column":"t_inner_lat","type":"float","nullable":true,"nested":{"type":"Tagged","storage":"columns","field":"t","path":["inner","lat"],"under_null":true,"present_with":["t_inner_lng"]}},{"name":"t_inner_lng","column":"t_inner_lng","type":"float","nullable":true,"nested":{"type":"Tagged","storage":"columns","field":"t","path":["inner","lng"],"under_null":true,"present_with":["t_inner_lat"]}}],"constraints":[{"kind":"check","expr":"(\"t_inner_lat\" IS NULL AND \"t_inner_lng\" IS NULL) OR (\"t_inner_lat\" IS NOT NULL)"},{"kind":"check","expr":"(\"t_tag\" IS NULL AND \"t_inner_lat\" IS NULL AND \"t_inner_lng\" IS NULL) OR (\"t_tag\" IS NOT NULL)"}]},{"name":"Slugged","table":"slugged","fields":[{"name":"id","column":"id","type":"int","primary_key":true,"index":true},{"name":"slug","column":"slug","type":"string"}],"indexes":[{"name":"slugged_slug","columns":[{"field":"slug"}],"unique":true},{"name":"slugged_slug_id","columns":[{"field":"slug"},{"field":"id"}],"unique":true}]},{"name":"Titled","table":"titled","fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"title","column":"title","type":"string"}],"indexes":[{"name":"titled_title","columns":[{"field":"title"}],"unique":true}]}],"types":[{"name":"Geo","storage":"columns","fields":[{"name":"lat","nullable":false,"leaf":{"name":"lat","column":"lat","type":"float"}},{"name":"lng","nullable":true,"leaf":{"name":"lng","column":"lng","type":"float","nullable":true}}]},{"name":"Address","storage":"columns","fields":[{"name":"city","nullable":false,"leaf":{"name":"city","column":"city","type":"string"}},{"name":"floor","nullable":false,"leaf":{"name":"floor","column":"floor","type":"int","default":7}},{"name":"big","nullable":false,"leaf":{"name":"big","column":"big","type":"big_int"}},{"name":"opened","nullable":false,"leaf":{"name":"opened","column":"opened","type":"date_time"}},{"name":"geo","nullable":true,"type":"Geo"}]},{"name":"Wrap","storage":"columns","fields":[{"name":"inner","nullable":true,"type":"Geo"}]},{"name":"Tagged","storage":"columns","fields":[{"name":"tag","nullable":false,"leaf":{"name":"tag","column":"tag","type":"string"}},{"name":"inner","nullable":true,"type":"Geo"}]}]};

const models = define(SCHEMA, { requiredCapabilities: ["reference-loading"] });

const nested = (models as unknown as { readonly $types: Record<string, unknown> }).$types;

// -- Geo -------------------------------------------------------------------------------

/** Immutable `Geo` value, compared with `equals()`. */
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

/** Immutable `Address` value, compared with `equals()`. */
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

// -- Wrap ------------------------------------------------------------------------------

/** Immutable `Wrap` value, compared with `equals()`. */
export interface Wrap {
  readonly inner: Geo | null;
  equals(other: unknown): boolean;
  toJSON(): Record<string, unknown>;
}

export interface WrapInput {
  readonly inner?: Geo | GeoInput | null;
}

export interface WrapPatch {
  readonly inner?: Geo | GeoPatch | null;
}

export interface WrapClass {
  new (values: WrapInput): Wrap;
}

/** The columns of a `Wrap` member: the whole value, and one column per subfield. */
export type WrapRef<S extends string, H extends readonly Hop[], O extends boolean, N extends string, X> = Column<O extends true ? X | null : X, S, N, H> & {
  readonly inner: GeoRef<S, H, O, "inner", Geo | null | (null extends X ? null : never)>;
};

export const Wrap = nested["Wrap"] as unknown as WrapClass;

// -- Tagged ----------------------------------------------------------------------------

/** Immutable `Tagged` value, compared with `equals()`. */
export interface Tagged {
  readonly tag: string;
  readonly inner: Geo | null;
  equals(other: unknown): boolean;
  toJSON(): Record<string, unknown>;
}

export interface TaggedInput {
  readonly tag: In<string>;
  readonly inner?: Geo | GeoInput | null;
}

export interface TaggedPatch {
  readonly tag?: In<string>;
  readonly inner?: Geo | GeoPatch | null;
}

export interface TaggedClass {
  new (values: TaggedInput): Tagged;
}

/** The columns of a `Tagged` member: the whole value, and one column per subfield. */
export type TaggedRef<S extends string, H extends readonly Hop[], O extends boolean, N extends string, X> = Column<O extends true ? X | null : X, S, N, H> & {
  readonly tag: Column<O extends true ? string | null : null extends X ? string | null : string, S, "tag", H>;
  readonly inner: GeoRef<S, H, O, "inner", Geo | null | (null extends X ? null : never)>;
};

export const Tagged = nested["Tagged"] as unknown as TaggedClass;

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

// -- Plain -----------------------------------------------------------------------------

/** The column values of a Plain row. */
export interface PlainData {
  readonly id: number;
  readonly g: Geo;
}

/** A Plain row. To-one relations are typed on rows of queries that load them. */
export interface Plain extends PlainData, Instance<PlainSpec> {
}

export type PlainInsert = {
  id: In<number>;
  g: Geo | GeoInput;
};

export interface PlainUpdate {
  id?: In<number> | Expression<Compat<number>, "Plain" | "~Plain", {}>;
  g?: Geo | GeoPatch;
}

export interface PlainUpdateRow {
  id: In<number>;
  g?: Geo | GeoInput;
}

export interface PlainSpec {
  readonly name: "Plain";
  readonly row: Plain;
  readonly data: PlainData;
  readonly insert: PlainInsert;
  readonly update: PlainUpdate;
  readonly updateRow: PlainUpdateRow;
  readonly pk: number;
}

export interface PlainFields<S extends string, H extends readonly Hop[], O extends boolean> {
  readonly id: Column<O extends true ? number | null : number, S, "id", H>;
  readonly g: GeoRef<S, H, O, "g", Geo>;
}

export interface PlainPath<S extends string, H extends readonly Hop[], O extends boolean>
  extends RelationPath<PlainSpec, S, H>,
    PlainFields<S, H, O> {
  /** The relation query set for `load()`: `Plain.objects` bound to this relation. */
  readonly objects: QuerySetOf<PlainSpec, Plain, "Plain", {}, never, Via<H, undefined, false, S>>;
}

export interface PlainModel extends ModelClass<PlainSpec>, PlainFields<"Plain", [], false> {}

export const Plain = models["Plain"] as unknown as PlainModel;

// -- Wrapped ---------------------------------------------------------------------------

/** The column values of a Wrapped row. */
export interface WrappedData {
  readonly id: number;
  readonly w: Wrap;
}

/** A Wrapped row. To-one relations are typed on rows of queries that load them. */
export interface Wrapped extends WrappedData, Instance<WrappedSpec> {
}

export type WrappedInsert = {
  id: In<number>;
  w?: Wrap | WrapInput;
};

export interface WrappedUpdate {
  id?: In<number> | Expression<Compat<number>, "Wrapped" | "~Wrapped", {}>;
  w?: Wrap | WrapPatch;
}

export interface WrappedUpdateRow {
  id: In<number>;
  w?: Wrap | WrapInput;
}

export interface WrappedSpec {
  readonly name: "Wrapped";
  readonly row: Wrapped;
  readonly data: WrappedData;
  readonly insert: WrappedInsert;
  readonly update: WrappedUpdate;
  readonly updateRow: WrappedUpdateRow;
  readonly pk: number;
}

export interface WrappedFields<S extends string, H extends readonly Hop[], O extends boolean> {
  readonly id: Column<O extends true ? number | null : number, S, "id", H>;
  readonly w: WrapRef<S, H, O, "w", Wrap>;
}

export interface WrappedPath<S extends string, H extends readonly Hop[], O extends boolean>
  extends RelationPath<WrappedSpec, S, H>,
    WrappedFields<S, H, O> {
  /** The relation query set for `load()`: `Wrapped.objects` bound to this relation. */
  readonly objects: QuerySetOf<WrappedSpec, Wrapped, "Wrapped", {}, never, Via<H, undefined, false, S>>;
}

export interface WrappedModel extends ModelClass<WrappedSpec>, WrappedFields<"Wrapped", [], false> {}

export const Wrapped = models["Wrapped"] as unknown as WrappedModel;

// -- Opt -------------------------------------------------------------------------------

/** The column values of a Opt row. */
export interface OptData {
  readonly id: number;
  readonly t: Tagged | null;
}

/** A Opt row. To-one relations are typed on rows of queries that load them. */
export interface Opt extends OptData, Instance<OptSpec> {
}

export type OptInsert = {
  id: In<number>;
  t?: Tagged | TaggedInput | null;
};

export interface OptUpdate {
  id?: In<number> | Expression<Compat<number>, "Opt" | "~Opt", {}>;
  t?: Tagged | TaggedPatch | null;
}

export interface OptUpdateRow {
  id: In<number>;
  t?: Tagged | TaggedInput | null;
}

export interface OptSpec {
  readonly name: "Opt";
  readonly row: Opt;
  readonly data: OptData;
  readonly insert: OptInsert;
  readonly update: OptUpdate;
  readonly updateRow: OptUpdateRow;
  readonly pk: number;
}

export interface OptFields<S extends string, H extends readonly Hop[], O extends boolean> {
  readonly id: Column<O extends true ? number | null : number, S, "id", H>;
  readonly t: TaggedRef<S, H, O, "t", Tagged | null>;
}

export interface OptPath<S extends string, H extends readonly Hop[], O extends boolean>
  extends RelationPath<OptSpec, S, H>,
    OptFields<S, H, O> {
  /** The relation query set for `load()`: `Opt.objects` bound to this relation. */
  readonly objects: QuerySetOf<OptSpec, Opt, "Opt", {}, never, Via<H, undefined, false, S>>;
}

export interface OptModel extends ModelClass<OptSpec>, OptFields<"Opt", [], false> {}

export const Opt = models["Opt"] as unknown as OptModel;

// -- Slugged ---------------------------------------------------------------------------

/** The column values of a Slugged row. */
export interface SluggedData {
  readonly id: number;
  readonly slug: string;
}

/** A Slugged row. To-one relations are typed on rows of queries that load them. */
export interface Slugged extends SluggedData, Instance<SluggedSpec> {
}

export type SluggedInsert = {
  id: In<number>;
  slug: In<string>;
};

export interface SluggedUpdate {
  id?: In<number> | Expression<Compat<number>, "Slugged" | "~Slugged", {}>;
  slug?: In<string> | Expression<Compat<string>, "Slugged" | "~Slugged", {}>;
}

export interface SluggedUpdateRow {
  id: In<number>;
  slug?: In<string>;
}

export interface SluggedSpec {
  readonly name: "Slugged";
  readonly row: Slugged;
  readonly data: SluggedData;
  readonly insert: SluggedInsert;
  readonly update: SluggedUpdate;
  readonly updateRow: SluggedUpdateRow;
  readonly pk: number;
}

export interface SluggedFields<S extends string, H extends readonly Hop[], O extends boolean> {
  readonly id: Column<O extends true ? number | null : number, S, "id", H>;
  readonly slug: Column<O extends true ? string | null : string, S, "slug", H>;
}

export interface SluggedPath<S extends string, H extends readonly Hop[], O extends boolean>
  extends RelationPath<SluggedSpec, S, H>,
    SluggedFields<S, H, O> {
  /** The relation query set for `load()`: `Slugged.objects` bound to this relation. */
  readonly objects: QuerySetOf<SluggedSpec, Slugged, "Slugged", {}, never, Via<H, undefined, false, S>>;
}

export interface SluggedModel extends ModelClass<SluggedSpec>, SluggedFields<"Slugged", [], false> {
  /** The unique indexes, for `onConflict(columns, { index })`. */
  readonly indexes: {
    readonly slugged_slug: IndexRef<"Slugged">;
    readonly slugged_slug_id: IndexRef<"Slugged">;
  };
}

export const Slugged = models["Slugged"] as unknown as SluggedModel;

// -- Titled ----------------------------------------------------------------------------

/** The column values of a Titled row. */
export interface TitledData {
  readonly id: number;
  readonly title: string;
}

/** A Titled row. To-one relations are typed on rows of queries that load them. */
export interface Titled extends TitledData, Instance<TitledSpec> {
}

export type TitledInsert = {
  id: In<number>;
  title: In<string>;
};

export interface TitledUpdate {
  id?: In<number> | Expression<Compat<number>, "Titled" | "~Titled", {}>;
  title?: In<string> | Expression<Compat<string>, "Titled" | "~Titled", {}>;
}

export interface TitledUpdateRow {
  id: In<number>;
  title?: In<string>;
}

export interface TitledSpec {
  readonly name: "Titled";
  readonly row: Titled;
  readonly data: TitledData;
  readonly insert: TitledInsert;
  readonly update: TitledUpdate;
  readonly updateRow: TitledUpdateRow;
  readonly pk: number;
}

export interface TitledFields<S extends string, H extends readonly Hop[], O extends boolean> {
  readonly id: Column<O extends true ? number | null : number, S, "id", H>;
  readonly title: Column<O extends true ? string | null : string, S, "title", H>;
}

export interface TitledPath<S extends string, H extends readonly Hop[], O extends boolean>
  extends RelationPath<TitledSpec, S, H>,
    TitledFields<S, H, O> {
  /** The relation query set for `load()`: `Titled.objects` bound to this relation. */
  readonly objects: QuerySetOf<TitledSpec, Titled, "Titled", {}, never, Via<H, undefined, false, S>>;
}

export interface TitledModel extends ModelClass<TitledSpec>, TitledFields<"Titled", [], false> {
  /** The unique indexes, for `onConflict(columns, { index })`. */
  readonly indexes: {
    readonly titled_title: IndexRef<"Titled">;
  };
}

export const Titled = models["Titled"] as unknown as TitledModel;

