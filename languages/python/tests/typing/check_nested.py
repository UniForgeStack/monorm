"""Static typing checks for nested types in the generated stub; run by languages/python/tests/test_typing.py
with mypy and pyright. Never executed."""

from datetime import datetime
from typing import assert_type

from nested.models import Address, Geo, Shop

from orm import ColumnRef, Condition


def read(shop: Shop) -> None:
    assert_type(shop.address, Address | None)
    assert_type(shop.snapshot, Address)
    assert_type(shop.snapshot.geo, Geo | None)
    assert_type(shop.snapshot.floor, int)
    shop.snapshot.floor = 1  # E: a value class is read-only
    shop.snapshot.nope  # E: unknown subfield
    shop.address_city  # E: a leaf name is not an attribute of the model


def references() -> None:
    assert_type(Shop.address.city, ColumnRef[str])
    assert_type(Shop.address.geo.lat, ColumnRef[float])
    assert_type(Shop.snapshot.geo.lng, ColumnRef[float | None])
    assert_type(Shop.address.city == "Oslo", Condition)
    Shop.address.nope  # E: unknown subfield


def values() -> None:
    Geo(lat=1.0)
    Address(city="Oslo", big=1, opened=datetime.now(), geo={"lat": 1.0})
    Address(city="Oslo", big=1, opened=datetime.now(), geo=None)
    Address(city=1, big=1, opened=datetime.now())  # E: city is a str
    Address(city="Oslo", big=1)  # E: opened is required


def writes() -> None:
    Shop.objects.insert(id=1, snapshot={"city": "Oslo", "big": 1, "opened": datetime.now()})
    Shop.objects.insert(id=1, address=None, snapshot=Address(city="Oslo", big=1, opened=datetime.now()))
    Shop.objects.insert(id=1, snapshot={"city": 1, "big": 1, "opened": datetime.now()})  # E: city is a str
    Shop.objects.insert(id=1)  # E: snapshot is required
    Shop.objects.insert(id=1, address_city="Oslo", snapshot={"city": "a", "big": 1, "opened": datetime.now()})  # E: a leaf name cannot be set
    Shop.objects.update(address={"floor": 3})
    Shop.objects.update(address={"geo": {"lng": None}})
    Shop.objects.update(address=None)
    Shop.objects.update(address={"floor": "x"})  # E: floor is an int
    Shop.objects.update(snapshot=None)  # E: snapshot is not nullable
    Shop.objects.update(address_city="x")  # E: a leaf name cannot be set
