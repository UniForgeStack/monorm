# Generated from schema.prisma by `python -m orm generate`. Do not edit.
#
# Static half of the generated module. Each model gets:
#   * the model class: column descriptors (`User.email` is a ColumnRef[str] on the class,
#     a read-only str on an instance), relation descriptors and typed `update()`;
#   * a path class (`_UserPath`): what a relation to the model evaluates to on the class
#     side, so `User.posts.created_at` autocompletes and type-checks as ColumnRef[datetime];
#   * `UserInsert` / `UserUpdate` / `UserUpdateRow` / `UserLookup` TypedDicts: the row
#     shapes accepted by insert / update / update_many / get_or_insert;
#   * a query set class (`UserQuerySet`): typed `insert()`, `insert_many()`,
#     `get_or_insert()`, `update()`, `update_many()`.

from collections.abc import Iterable, Sequence
from datetime import datetime
from typing import Any, ClassVar, Literal, NotRequired, Required, TypedDict, overload

from typing_extensions import Self, Unpack

from orm import ColumnRef, CopyInsert, Expression, InsertMany, InsertOne, Model, NestedRef, NestedValue, QuerySet, RelationPath, Update, UpdateMany
from orm import fields as f

# -- Geo --------------------------------------------------------------------------------

class Geo(NestedValue):
    @property
    def lat(self) -> float: ...
    @property
    def lng(self) -> float | None: ...
    def __init__(self, *, lat: float, lng: float | None = ...) -> None: ...

class GeoInput(TypedDict):
    lat: float
    lng: NotRequired[float | None]

class GeoPatch(TypedDict, total=False):
    lat: float
    lng: float | None

class _GeoRef(NestedRef):
    lat: ColumnRef[float]
    lng: ColumnRef[float | None]

# -- Address ----------------------------------------------------------------------------

class Address(NestedValue):
    @property
    def city(self) -> str: ...
    @property
    def floor(self) -> int: ...
    @property
    def big(self) -> int: ...
    @property
    def opened(self) -> datetime: ...
    @property
    def geo(self) -> Geo | None: ...
    def __init__(self, *, city: str, floor: int = ..., big: int, opened: datetime, geo: Geo | GeoInput | None = ...) -> None: ...

class AddressInput(TypedDict):
    city: str
    floor: NotRequired[int]
    big: int
    opened: datetime
    geo: NotRequired[Geo | GeoInput | None]

class AddressPatch(TypedDict, total=False):
    city: str
    floor: int
    big: int
    opened: datetime
    geo: Geo | GeoPatch | None

class _AddressRef(NestedRef):
    city: ColumnRef[str]
    floor: ColumnRef[int]
    big: ColumnRef[int]
    opened: ColumnRef[datetime]
    geo: _GeoRef

# -- Shop -------------------------------------------------------------------------------

class Shop(Model):
    id: f.Integer[int]
    address: f.Nested[Address | None, _AddressRef]
    snapshot: f.Nested[Address, _AddressRef]

    objects: ClassVar[ShopQuerySet]

    async def update(self, **values: Unpack[ShopUpdate]) -> None: ...  # type: ignore[override]

class _ShopPath(RelationPath[Shop]):
    id: ColumnRef[int]
    address: _AddressRef
    snapshot: _AddressRef
    @property
    def objects(self) -> ShopQuerySet: ...

class ShopInsert(TypedDict):
    id: int
    address: NotRequired[Address | AddressInput | None]
    snapshot: Address | AddressInput

class ShopUpdate(TypedDict, total=False):
    id: int | Expression[int]
    address: Address | AddressPatch | None
    snapshot: Address | AddressPatch

class ShopUpdateRow(TypedDict, total=False):
    id: Required[int]
    address: Address | AddressInput | None
    snapshot: Address | AddressInput

class ShopLookup(TypedDict, total=False):
    id: int
    address: Address | AddressInput | None
    snapshot: Address | AddressInput

class ShopQuerySet(QuerySet[Shop]):
    def insert(self, **values: Unpack[ShopInsert]) -> InsertOne[Shop]: ...  # type: ignore[override]
    @overload  # type: ignore[override]
    def insert_many(self, rows: Iterable[ShopInsert], *, batch_size: int | None = None, copy: Literal[False] = False) -> InsertMany[Shop]: ...
    @overload
    def insert_many(self, rows: Iterable[ShopInsert], *, copy: Literal[True]) -> CopyInsert[Shop]: ...
    async def get_or_insert(self, defaults: ShopLookup | None = None, **lookup: Unpack[ShopLookup]) -> tuple[Shop, bool]: ...  # type: ignore[override]
    def update(self, **values: Unpack[ShopUpdate]) -> Update[Shop]: ...  # type: ignore[override]
    def update_many(self, rows: Iterable[ShopUpdateRow], *, batch_size: int | None = None) -> UpdateMany[Shop]: ...  # type: ignore[override]

__all__ = [
    "Geo",
    "GeoInput",
    "GeoPatch",
    "Address",
    "AddressInput",
    "AddressPatch",
    "Shop",
    "ShopInsert",
    "ShopUpdate",
    "ShopUpdateRow",
    "ShopQuerySet",
]
