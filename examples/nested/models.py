# Generated from schema.prisma by `python -m orm generate`. Do not edit.
#
# Runtime half of the generated module: the model classes are built from the compiled
# schema below. models.pyi carries the static types (columns, relation paths, typed
# inserts / updates and query sets) for editors and type checkers.

from orm import QuerySet, define

_SCHEMA = r"""
{"models":[{"name":"Shop","table":"shop","fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"address_city","column":"address_city","type":"string","nullable":true,"nested":{"type":"Address","storage":"columns","field":"address","path":["city"],"under_null":true,"present_with":["address_floor","address_big","address_opened","address_geo_lat","address_geo_lng"]}},{"name":"address_floor","column":"address_floor","type":"int","nullable":true,"default":7,"nested":{"type":"Address","storage":"columns","field":"address","path":["floor"],"under_null":true,"present_with":["address_city","address_big","address_opened","address_geo_lat","address_geo_lng"]}},{"name":"address_big","column":"address_big","type":"big_int","nullable":true,"nested":{"type":"Address","storage":"columns","field":"address","path":["big"],"under_null":true,"present_with":["address_city","address_floor","address_opened","address_geo_lat","address_geo_lng"]}},{"name":"address_opened","column":"address_opened","type":"date_time","nullable":true,"nested":{"type":"Address","storage":"columns","field":"address","path":["opened"],"under_null":true,"present_with":["address_city","address_floor","address_big","address_geo_lat","address_geo_lng"]}},{"name":"address_geo_lat","column":"address_geo_lat","type":"float","nullable":true,"nested":{"type":"Address","storage":"columns","field":"address","path":["geo","lat"],"under_null":true,"present_with":["address_geo_lng"]}},{"name":"address_geo_lng","column":"address_geo_lng","type":"float","nullable":true,"nested":{"type":"Address","storage":"columns","field":"address","path":["geo","lng"],"under_null":true,"present_with":["address_geo_lat"]}},{"name":"snapshot","column":"snapshot","type":"json","nested":{"type":"Address","storage":"json","field":"snapshot"}}],"constraints":[{"kind":"check","expr":"(\"address_geo_lat\" IS NULL AND \"address_geo_lng\" IS NULL) OR (\"address_geo_lat\" IS NOT NULL)"},{"kind":"check","expr":"(\"address_city\" IS NULL AND \"address_floor\" IS NULL AND \"address_big\" IS NULL AND \"address_opened\" IS NULL AND \"address_geo_lat\" IS NULL AND \"address_geo_lng\" IS NULL) OR (\"address_city\" IS NOT NULL AND \"address_floor\" IS NOT NULL AND \"address_big\" IS NOT NULL AND \"address_opened\" IS NOT NULL)"}]},{"name":"Plain","table":"plain","fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"g_lat","column":"g_lat","type":"float","nested":{"type":"Geo","storage":"columns","field":"g","path":["lat"],"root_required":true}},{"name":"g_lng","column":"g_lng","type":"float","nullable":true,"nested":{"type":"Geo","storage":"columns","field":"g","path":["lng"],"root_required":true}}]},{"name":"Wrapped","table":"wrapped","fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"w_inner_lat","column":"w_inner_lat","type":"float","nullable":true,"nested":{"type":"Wrap","storage":"columns","field":"w","path":["inner","lat"],"under_null":true,"root_required":true,"present_with":["w_inner_lng"]}},{"name":"w_inner_lng","column":"w_inner_lng","type":"float","nullable":true,"nested":{"type":"Wrap","storage":"columns","field":"w","path":["inner","lng"],"under_null":true,"root_required":true,"present_with":["w_inner_lat"]}}],"constraints":[{"kind":"check","expr":"(\"w_inner_lat\" IS NULL AND \"w_inner_lng\" IS NULL) OR (\"w_inner_lat\" IS NOT NULL)"}]},{"name":"Opt","table":"opt","fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"t_tag","column":"t_tag","type":"string","nullable":true,"nested":{"type":"Tagged","storage":"columns","field":"t","path":["tag"],"under_null":true,"present_with":["t_inner_lat","t_inner_lng"]}},{"name":"t_inner_lat","column":"t_inner_lat","type":"float","nullable":true,"nested":{"type":"Tagged","storage":"columns","field":"t","path":["inner","lat"],"under_null":true,"present_with":["t_inner_lng"]}},{"name":"t_inner_lng","column":"t_inner_lng","type":"float","nullable":true,"nested":{"type":"Tagged","storage":"columns","field":"t","path":["inner","lng"],"under_null":true,"present_with":["t_inner_lat"]}}],"constraints":[{"kind":"check","expr":"(\"t_inner_lat\" IS NULL AND \"t_inner_lng\" IS NULL) OR (\"t_inner_lat\" IS NOT NULL)"},{"kind":"check","expr":"(\"t_tag\" IS NULL AND \"t_inner_lat\" IS NULL AND \"t_inner_lng\" IS NULL) OR (\"t_tag\" IS NOT NULL)"}]}],"types":[{"name":"Geo","storage":"columns","fields":[{"name":"lat","nullable":false,"leaf":{"name":"lat","column":"lat","type":"float"}},{"name":"lng","nullable":true,"leaf":{"name":"lng","column":"lng","type":"float","nullable":true}}]},{"name":"Address","storage":"columns","fields":[{"name":"city","nullable":false,"leaf":{"name":"city","column":"city","type":"string"}},{"name":"floor","nullable":false,"leaf":{"name":"floor","column":"floor","type":"int","default":7}},{"name":"big","nullable":false,"leaf":{"name":"big","column":"big","type":"big_int"}},{"name":"opened","nullable":false,"leaf":{"name":"opened","column":"opened","type":"date_time"}},{"name":"geo","nullable":true,"type":"Geo"}]},{"name":"Wrap","storage":"columns","fields":[{"name":"inner","nullable":true,"type":"Geo"}]},{"name":"Tagged","storage":"columns","fields":[{"name":"tag","nullable":false,"leaf":{"name":"tag","column":"tag","type":"string"}},{"name":"inner","nullable":true,"type":"Geo"}]}]}
"""

_models = define(_SCHEMA, module=__name__, required_capabilities=("reference-loading",))
Geo = _models["Geo"]
Address = _models["Address"]
Wrap = _models["Wrap"]
Tagged = _models["Tagged"]
Shop = _models["Shop"]
Plain = _models["Plain"]
Wrapped = _models["Wrapped"]
Opt = _models["Opt"]
GeoInput = GeoPatch = AddressInput = AddressPatch = WrapInput = WrapPatch = TaggedInput = TaggedPatch = dict

# Typed per model in models.pyi; plain aliases at runtime so the names can be imported.
ShopQuerySet = PlainQuerySet = WrappedQuerySet = OptQuerySet = QuerySet
ShopInsert = ShopUpdate = ShopUpdateRow = PlainInsert = PlainUpdate = PlainUpdateRow = WrappedInsert = WrappedUpdate = WrappedUpdateRow = OptInsert = OptUpdate = OptUpdateRow = dict

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
    "ShopQuerySet",
    "PlainQuerySet",
    "WrappedQuerySet",
    "OptQuerySet",
]
