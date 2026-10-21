"""Nested fields at the bulk entry points: update_many replaces, get_or_insert looks up plain fields."""

import pytest

from test_nested import address, seed, shop, world  # noqa: F401
from orm.write import lookup_values


async def test_update_many_refuses_a_dict_for_a_nested_field(shop):
    q, S, classes, _ = shop
    await seed(shop)
    before = {s.id: s.address for s in await q.order_by(S.id)}
    partial = {"city": "Partial"}
    full = {"city": "Full", "big": 1, "open": False, "opened": address(classes).opened}
    for given in (partial, full):
        with pytest.raises(TypeError, match=r"address is a dict.*update\(\)"):
            await q.update_many([{"id": 1, "address": given}])
    assert {s.id: s.address for s in await q.order_by(S.id)} == before
    await q.update_many([{"id": 1, "address": address(classes, city="Many")}, {"id": 2, "address": None}])
    rows = {s.id: s.address for s in await q.order_by(S.id)}
    assert rows[1].city == "Many"
    assert rows[2] is None


async def test_get_or_insert_rejects_a_nested_key(shop):
    q, S, classes, _ = shop
    for key in (address(classes), {"city": "Oslo"}, None):
        with pytest.raises(TypeError, match=r"address is a nested field"):
            await q.get_or_insert(address=key)


def test_get_or_insert_keeps_a_leaf_column_name_valid(world):
    _, classes, _ = world
    meta = classes["CShop"]._meta
    leaf = next(iter(meta.leaf_group))
    assert lookup_values(classes["CShop"], {leaf: "x"}) == {leaf: "x"}
