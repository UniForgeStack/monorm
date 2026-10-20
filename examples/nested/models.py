# Generated from schema.prisma by `python -m orm generate`. Do not edit.
#
# Runtime half of the generated module: the model classes are built from the compiled
# schema below. models.pyi carries the static types (columns, relation paths, typed
# inserts / updates and query sets) for editors and type checkers.

from orm import QuerySet, define

_SCHEMA = r"""
{"models":[{"name":"Shop","table":"shop","fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"address_city","column":"address_city","type":"string","nullable":true,"nested":{"type":"Address","storage":"columns","field":"address","path":["city"],"under_null":true,"present_with":["address_floor","address_big","address_opened","address_geo_lat","address_geo_lng"]}},{"name":"address_floor","column":"address_floor","type":"int","nullable":true,"default":7,"nested":{"type":"Address","storage":"columns","field":"address","path":["floor"],"under_null":true,"present_with":["address_city","address_big","address_opened","address_geo_lat","address_geo_lng"]}},{"name":"address_big","column":"address_big","type":"big_int","nullable":true,"nested":{"type":"Address","storage":"columns","field":"address","path":["big"],"under_null":true,"present_with":["address_city","address_floor","address_opened","address_geo_lat","address_geo_lng"]}},{"name":"address_opened","column":"address_opened","type":"date_time","nullable":true,"nested":{"type":"Address","storage":"columns","field":"address","path":["opened"],"under_null":true,"present_with":["address_city","address_floor","address_big","address_geo_lat","address_geo_lng"]}},{"name":"address_geo_lat","column":"address_geo_lat","type":"float","nullable":true,"nested":{"type":"Address","storage":"columns","field":"address","path":["geo","lat"],"under_null":true,"present_with":["address_geo_lng"]}},{"name":"address_geo_lng","column":"address_geo_lng","type":"float","nullable":true,"nested":{"type":"Address","storage":"columns","field":"address","path":["geo","lng"],"under_null":true,"present_with":["address_geo_lat"]}},{"name":"snapshot","column":"snapshot","type":"json","nested":{"type":"Address","storage":"json","field":"snapshot"}}],"constraints":[{"kind":"check","expr":"(\"address_geo_lat\" IS NULL AND \"address_geo_lng\" IS NULL) OR (\"address_geo_lat\" IS NOT NULL)"},{"kind":"check","expr":"(\"address_city\" IS NULL AND \"address_floor\" IS NULL AND \"address_big\" IS NULL AND \"address_opened\" IS NULL AND \"address_geo_lat\" IS NULL AND \"address_geo_lng\" IS NULL) OR (\"address_city\" IS NOT NULL AND \"address_floor\" IS NOT NULL AND \"address_big\" IS NOT NULL AND \"address_opened\" IS NOT NULL)"}]}],"types":[{"name":"Geo","storage":"columns","fields":[{"name":"lat","nullable":false,"leaf":{"name":"lat","column":"lat","type":"float"}},{"name":"lng","nullable":true,"leaf":{"name":"lng","column":"lng","type":"float","nullable":true}}]},{"name":"Address","storage":"columns","fields":[{"name":"city","nullable":false,"leaf":{"name":"city","column":"city","type":"string"}},{"name":"floor","nullable":false,"leaf":{"name":"floor","column":"floor","type":"int","default":7}},{"name":"big","nullable":false,"leaf":{"name":"big","column":"big","type":"big_int"}},{"name":"opened","nullable":false,"leaf":{"name":"opened","column":"opened","type":"date_time"}},{"name":"geo","nullable":true,"type":"Geo"}]}]}
"""

_models = define(_SCHEMA, module=__name__, required_capabilities=("reference-loading",))
Geo = _models["Geo"]
Address = _models["Address"]
Shop = _models["Shop"]
GeoInput = GeoPatch = AddressInput = AddressPatch = dict

# Typed per model in models.pyi; plain aliases at runtime so the names can be imported.
ShopQuerySet = QuerySet
ShopInsert = ShopUpdate = ShopUpdateRow = dict

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
