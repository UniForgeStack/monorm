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

from collections.abc import Iterable
from datetime import datetime
from typing import Any, ClassVar, Literal, NotRequired, Required, TypedDict, overload

from typing_extensions import Self, Unpack

from orm import ColumnRef, CopyInsert, Expression, InsertMany, InsertOne, Model, IndexRef, NestedRef, NestedValue, QuerySet, RelationPath, Update, UpdateMany
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

# -- Wrap -------------------------------------------------------------------------------

class Wrap(NestedValue):
    @property
    def inner(self) -> Geo | None: ...
    def __init__(self, *, inner: Geo | GeoInput | None = ...) -> None: ...

class WrapInput(TypedDict):
    inner: NotRequired[Geo | GeoInput | None]

class WrapPatch(TypedDict, total=False):
    inner: Geo | GeoPatch | None

class _WrapRef(NestedRef):
    inner: _GeoRef

# -- Tagged -----------------------------------------------------------------------------

class Tagged(NestedValue):
    @property
    def tag(self) -> str: ...
    @property
    def inner(self) -> Geo | None: ...
    def __init__(self, *, tag: str, inner: Geo | GeoInput | None = ...) -> None: ...

class TaggedInput(TypedDict):
    tag: str
    inner: NotRequired[Geo | GeoInput | None]

class TaggedPatch(TypedDict, total=False):
    tag: str
    inner: Geo | GeoPatch | None

class _TaggedRef(NestedRef):
    tag: ColumnRef[str]
    inner: _GeoRef

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

# -- Plain ------------------------------------------------------------------------------

class Plain(Model):
    id: f.Integer[int]
    g: f.Nested[Geo, _GeoRef]

    objects: ClassVar[PlainQuerySet]

    async def update(self, **values: Unpack[PlainUpdate]) -> None: ...  # type: ignore[override]

class _PlainPath(RelationPath[Plain]):
    id: ColumnRef[int]
    g: _GeoRef
    @property
    def objects(self) -> PlainQuerySet: ...

class PlainInsert(TypedDict):
    id: int
    g: Geo | GeoInput

class PlainUpdate(TypedDict, total=False):
    id: int | Expression[int]
    g: Geo | GeoPatch

class PlainUpdateRow(TypedDict, total=False):
    id: Required[int]
    g: Geo | GeoInput

class PlainLookup(TypedDict, total=False):
    id: int
    g: Geo | GeoInput

class PlainQuerySet(QuerySet[Plain]):
    def insert(self, **values: Unpack[PlainInsert]) -> InsertOne[Plain]: ...  # type: ignore[override]
    @overload  # type: ignore[override]
    def insert_many(self, rows: Iterable[PlainInsert], *, batch_size: int | None = None, copy: Literal[False] = False) -> InsertMany[Plain]: ...
    @overload
    def insert_many(self, rows: Iterable[PlainInsert], *, copy: Literal[True]) -> CopyInsert[Plain]: ...
    async def get_or_insert(self, defaults: PlainLookup | None = None, **lookup: Unpack[PlainLookup]) -> tuple[Plain, bool]: ...  # type: ignore[override]
    def update(self, **values: Unpack[PlainUpdate]) -> Update[Plain]: ...  # type: ignore[override]
    def update_many(self, rows: Iterable[PlainUpdateRow], *, batch_size: int | None = None) -> UpdateMany[Plain]: ...  # type: ignore[override]

# -- Wrapped ----------------------------------------------------------------------------

class Wrapped(Model):
    id: f.Integer[int]
    w: f.Nested[Wrap, _WrapRef]

    objects: ClassVar[WrappedQuerySet]

    async def update(self, **values: Unpack[WrappedUpdate]) -> None: ...  # type: ignore[override]

class _WrappedPath(RelationPath[Wrapped]):
    id: ColumnRef[int]
    w: _WrapRef
    @property
    def objects(self) -> WrappedQuerySet: ...

class WrappedInsert(TypedDict):
    id: int
    w: NotRequired[Wrap | WrapInput]

class WrappedUpdate(TypedDict, total=False):
    id: int | Expression[int]
    w: Wrap | WrapPatch

class WrappedUpdateRow(TypedDict, total=False):
    id: Required[int]
    w: Wrap | WrapInput

class WrappedLookup(TypedDict, total=False):
    id: int
    w: Wrap | WrapInput

class WrappedQuerySet(QuerySet[Wrapped]):
    def insert(self, **values: Unpack[WrappedInsert]) -> InsertOne[Wrapped]: ...  # type: ignore[override]
    @overload  # type: ignore[override]
    def insert_many(self, rows: Iterable[WrappedInsert], *, batch_size: int | None = None, copy: Literal[False] = False) -> InsertMany[Wrapped]: ...
    @overload
    def insert_many(self, rows: Iterable[WrappedInsert], *, copy: Literal[True]) -> CopyInsert[Wrapped]: ...
    async def get_or_insert(self, defaults: WrappedLookup | None = None, **lookup: Unpack[WrappedLookup]) -> tuple[Wrapped, bool]: ...  # type: ignore[override]
    def update(self, **values: Unpack[WrappedUpdate]) -> Update[Wrapped]: ...  # type: ignore[override]
    def update_many(self, rows: Iterable[WrappedUpdateRow], *, batch_size: int | None = None) -> UpdateMany[Wrapped]: ...  # type: ignore[override]

# -- Opt --------------------------------------------------------------------------------

class Opt(Model):
    id: f.Integer[int]
    t: f.Nested[Tagged | None, _TaggedRef]

    objects: ClassVar[OptQuerySet]

    async def update(self, **values: Unpack[OptUpdate]) -> None: ...  # type: ignore[override]

class _OptPath(RelationPath[Opt]):
    id: ColumnRef[int]
    t: _TaggedRef
    @property
    def objects(self) -> OptQuerySet: ...

class OptInsert(TypedDict):
    id: int
    t: NotRequired[Tagged | TaggedInput | None]

class OptUpdate(TypedDict, total=False):
    id: int | Expression[int]
    t: Tagged | TaggedPatch | None

class OptUpdateRow(TypedDict, total=False):
    id: Required[int]
    t: Tagged | TaggedInput | None

class OptLookup(TypedDict, total=False):
    id: int
    t: Tagged | TaggedInput | None

class OptQuerySet(QuerySet[Opt]):
    def insert(self, **values: Unpack[OptInsert]) -> InsertOne[Opt]: ...  # type: ignore[override]
    @overload  # type: ignore[override]
    def insert_many(self, rows: Iterable[OptInsert], *, batch_size: int | None = None, copy: Literal[False] = False) -> InsertMany[Opt]: ...
    @overload
    def insert_many(self, rows: Iterable[OptInsert], *, copy: Literal[True]) -> CopyInsert[Opt]: ...
    async def get_or_insert(self, defaults: OptLookup | None = None, **lookup: Unpack[OptLookup]) -> tuple[Opt, bool]: ...  # type: ignore[override]
    def update(self, **values: Unpack[OptUpdate]) -> Update[Opt]: ...  # type: ignore[override]
    def update_many(self, rows: Iterable[OptUpdateRow], *, batch_size: int | None = None) -> UpdateMany[Opt]: ...  # type: ignore[override]

# -- Slugged ----------------------------------------------------------------------------

class Slugged(Model):
    id: f.Integer[int]
    slug: f.String[str]

    objects: ClassVar[SluggedQuerySet]
    indexes: ClassVar[_SluggedIndexes]

    async def update(self, **values: Unpack[SluggedUpdate]) -> None: ...  # type: ignore[override]

class _SluggedPath(RelationPath[Slugged]):
    id: ColumnRef[int]
    slug: ColumnRef[str]
    @property
    def objects(self) -> SluggedQuerySet: ...

class _SluggedIndexes:
    slugged_slug: IndexRef
    slugged_slug_id: IndexRef
    def __getitem__(self, name: str) -> IndexRef: ...

class SluggedInsert(TypedDict):
    id: int
    slug: str

class SluggedUpdate(TypedDict, total=False):
    id: int | Expression[int]
    slug: str | Expression[str]

class SluggedUpdateRow(TypedDict, total=False):
    id: Required[int]
    slug: str

class SluggedLookup(TypedDict, total=False):
    id: int
    slug: str

class SluggedQuerySet(QuerySet[Slugged]):
    def insert(self, **values: Unpack[SluggedInsert]) -> InsertOne[Slugged]: ...  # type: ignore[override]
    @overload  # type: ignore[override]
    def insert_many(self, rows: Iterable[SluggedInsert], *, batch_size: int | None = None, copy: Literal[False] = False) -> InsertMany[Slugged]: ...
    @overload
    def insert_many(self, rows: Iterable[SluggedInsert], *, copy: Literal[True]) -> CopyInsert[Slugged]: ...
    async def get_or_insert(self, defaults: SluggedLookup | None = None, **lookup: Unpack[SluggedLookup]) -> tuple[Slugged, bool]: ...  # type: ignore[override]
    def update(self, **values: Unpack[SluggedUpdate]) -> Update[Slugged]: ...  # type: ignore[override]
    def update_many(self, rows: Iterable[SluggedUpdateRow], *, batch_size: int | None = None) -> UpdateMany[Slugged]: ...  # type: ignore[override]

# -- Titled -----------------------------------------------------------------------------

class Titled(Model):
    id: f.Integer[int]
    title: f.String[str]

    objects: ClassVar[TitledQuerySet]
    indexes: ClassVar[_TitledIndexes]

    async def update(self, **values: Unpack[TitledUpdate]) -> None: ...  # type: ignore[override]

class _TitledPath(RelationPath[Titled]):
    id: ColumnRef[int]
    title: ColumnRef[str]
    @property
    def objects(self) -> TitledQuerySet: ...

class _TitledIndexes:
    titled_title: IndexRef
    def __getitem__(self, name: str) -> IndexRef: ...

class TitledInsert(TypedDict):
    id: int
    title: str

class TitledUpdate(TypedDict, total=False):
    id: int | Expression[int]
    title: str | Expression[str]

class TitledUpdateRow(TypedDict, total=False):
    id: Required[int]
    title: str

class TitledLookup(TypedDict, total=False):
    id: int
    title: str

class TitledQuerySet(QuerySet[Titled]):
    def insert(self, **values: Unpack[TitledInsert]) -> InsertOne[Titled]: ...  # type: ignore[override]
    @overload  # type: ignore[override]
    def insert_many(self, rows: Iterable[TitledInsert], *, batch_size: int | None = None, copy: Literal[False] = False) -> InsertMany[Titled]: ...
    @overload
    def insert_many(self, rows: Iterable[TitledInsert], *, copy: Literal[True]) -> CopyInsert[Titled]: ...
    async def get_or_insert(self, defaults: TitledLookup | None = None, **lookup: Unpack[TitledLookup]) -> tuple[Titled, bool]: ...  # type: ignore[override]
    def update(self, **values: Unpack[TitledUpdate]) -> Update[Titled]: ...  # type: ignore[override]
    def update_many(self, rows: Iterable[TitledUpdateRow], *, batch_size: int | None = None) -> UpdateMany[Titled]: ...  # type: ignore[override]

__all__ = [
    "Geo",
    "GeoInput",
    "GeoPatch",
    "Address",
    "AddressInput",
    "AddressPatch",
    "Wrap",
    "WrapInput",
    "WrapPatch",
    "Tagged",
    "TaggedInput",
    "TaggedPatch",
    "Shop",
    "Plain",
    "Wrapped",
    "Opt",
    "Slugged",
    "Titled",
    "ShopInsert",
    "ShopUpdate",
    "ShopUpdateRow",
    "PlainInsert",
    "PlainUpdate",
    "PlainUpdateRow",
    "WrappedInsert",
    "WrappedUpdate",
    "WrappedUpdateRow",
    "OptInsert",
    "OptUpdate",
    "OptUpdateRow",
    "SluggedInsert",
    "SluggedUpdate",
    "SluggedUpdateRow",
    "TitledInsert",
    "TitledUpdate",
    "TitledUpdateRow",
    "ShopQuerySet",
    "PlainQuerySet",
    "WrappedQuerySet",
    "OptQuerySet",
    "SluggedQuerySet",
    "TitledQuerySet",
]
