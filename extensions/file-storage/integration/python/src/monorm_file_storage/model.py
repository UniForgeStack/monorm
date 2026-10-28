"""Selected model setup shared by generated and runtime-defined bindings."""

from collections.abc import Mapping, Sequence
from typing import Any
from . import FileAdapter, FileField, FileFieldError, Registry
from .decoder import prepare_decoder


class ModelAdapter:
    def __init__(self, fields: Mapping[str, FileField], registry: Registry | None = None):
        self.fields = dict(fields)
        self.registry = registry

    def configure(self, registry: Registry) -> None:
        self.registry = registry

    def decoder(self, public: Sequence[tuple[str, int]]) -> Any:
        return prepare_decoder(self.fields, public)


def install_model(model: type[Any], fields: Mapping[str, FileField], registry: Registry | None = None) -> ModelAdapter:
    """Called once at definition after selected artifact validation, before publish.

    Each file field registers as an async field adapter, which runs the upload before the SQL
    and decodes a loaded field to a ``Reference``.
    """
    adapter = ModelAdapter(fields, registry)
    methods: dict[str, Any] = {}
    for name, field in fields.items():

        def signed_url(self: Any, *, expires_in: int = 300, _field: FileField = field) -> Any:
            reference = _field.reference(vars(self))
            if adapter.registry is None:
                raise FileFieldError("configure file storage before file operations")
            return adapter.registry.signed_url(reference, expires_in=expires_in)

        def open_file(self: Any, _field: FileField = field) -> Any:
            reference = _field.reference(vars(self))
            if adapter.registry is None:
                raise FileFieldError("configure file storage before file operations")
            return adapter.registry.open(reference)

        methods[name + "_signed_url"] = signed_url
        methods[name + "_open"] = open_file
    # Validate the whole installation before attaching any method.
    for name in methods:
        if any(name in vars(base) for base in model.__mro__):
            raise FileFieldError(f"{model.__name__}.{name}: file method collision")
    for name, method in methods.items():
        setattr(model, name, method)
    if hasattr(model, "_meta"):
        from monorm.adapters import register_field_adapter

        for name, field in fields.items():
            register_field_adapter(model, name, FileAdapter(field, lambda: adapter.registry))
    return adapter


def install(model: type[Any], fields: Mapping[str, tuple[Mapping[str, Any], bool]]) -> ModelAdapter:
    """The generated-code entry for the `file-storage.reference.v1` adapter: `fields` maps a name to its options and nullability."""
    return install_model(model, {name: FileField(name, options["storage"], nullable) for name, (options, nullable) in fields.items()})
