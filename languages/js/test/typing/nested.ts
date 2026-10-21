/** Static checks of the generated nested types; never run. */

import { Address, Geo, Opt, Plain, Shop, Tagged, Wrap, Wrapped, type AddressPatch } from "../nested/models.js";

type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;
declare function same<A, B>(...check: Equal<A, B> extends true ? [] : [error: "types differ"]): void;
declare const shop: Shop;
declare const plain: Plain;
declare const wrapped: Wrapped;
declare const opt: Opt;

export async function nested() {
  same<typeof shop.address, Address | null>();
  same<typeof shop.snapshot, Address>();
  same<typeof shop.snapshot.geo, Geo | null>();
  same<typeof shop.snapshot.opened, Date>();
  same<typeof shop.snapshot.big, bigint>();
  const address = new Address({ city: "Oslo", big: 1, opened: new Date() });
  same<typeof address, Address>();
  await Shop.objects.insert({ id: 1, snapshot: address });
  await Shop.objects.insert({ id: 2, snapshot: { city: "Rome", big: 2n, opened: new Date(), geo: { lat: 1 } }, address: null });
  // @ts-expect-error a required subfield is missing
  await Shop.objects.insert({ id: 3, snapshot: { city: "Rome" } });
  // @ts-expect-error a subfield has the wrong type
  await Shop.objects.insert({ id: 3, snapshot: new Address({ city: 1, big: 1, opened: new Date() }) });
  await Shop.objects.filter(Shop.address.city.eq("Oslo")).orderBy(Shop.snapshot.geo.lat.desc());
  await Shop.objects.filter(Shop.snapshot.eq(address), Shop.address.isNull());
  // @ts-expect-error a subfield is typed
  Shop.address.city.eq(1);
  // @ts-expect-error a subfield that does not exist
  Shop.address.zip;
  const patch: AddressPatch = { city: "Bergen", geo: { lat: 2 } };
  await Shop.objects.update({ address: patch, snapshot: { floor: 2 } });
  await Shop.objects.update({ address: null });
  // @ts-expect-error the nested field `snapshot` is not nullable
  await Shop.objects.update({ snapshot: null });
  // @ts-expect-error a leaf name of a columns group is not public
  await Shop.objects.update({ addressCity: "x" });
  const rows = await Shop.objects.select({ city: Shop.address.city, whole: Shop.snapshot });
  same<typeof rows[number]["whole"], Address>();
  same<typeof rows[number]["city"], string | null>();
  same<typeof plain.g, Geo>();
  same<typeof wrapped.w, Wrap>();
  same<typeof opt.t, Tagged | null>();
  const lats = await Wrapped.objects.select({ lat: Wrapped.w.inner.lat });
  same<typeof lats[number]["lat"], number | null>();
}
