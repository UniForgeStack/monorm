"""`monorm.ValidationError`: messages per field path, and messages with no field."""

import pickle

import pytest
import monorm
from monorm import _native

SCHEMA = """datasource db { provider = "sqlite" }
model Doc {
  id    Int @id
  views Int @default(0)
}"""

# (fields, messages with no field, the display text): the same table is in the TypeScript twin.
DISPLAY_TEXTS = [
    ({}, [], "validation failed"),
    ({}, ["m1", "m2"], "m1; m2"),
    ({"a": ["x", "y"], "b.2.c": ["z"]}, [], "a: x; a: y; b.2.c: z"),
    ({"a": ["x"]}, ["m"], "m; a: x"),
]


def test_a_field_path_addresses_a_nested_field_and_a_list_item():
    error = monorm.ValidationError({"address.city": "is required"})
    error.add(("lines", 2, "qty"), "must be positive").add("lines.2.qty", "is too large")
    assert error.fields == {
        "address.city": ["is required"],
        "lines.2.qty": ["must be positive", "is too large"],
    }
    assert error.non_field_messages == []


def test_a_field_takes_one_message_or_a_list():
    error = monorm.ValidationError({"status": "is locked", "name": ["is empty", "is short"]})
    assert error.fields == {"status": ["is locked"], "name": ["is empty", "is short"]}


def test_messages_with_no_field_stay_apart_from_the_fields():
    error = monorm.ValidationError("The order is locked.")
    error.add_non_field_message("Try again later.")
    assert error.fields == {}
    assert error.non_field_messages == ["The order is locked.", "Try again later."]
    both = monorm.ValidationError({"a": "x"}, non_field_messages=["m"])
    assert (both.fields, both.non_field_messages) == ({"a": ["x"]}, ["m"])


def test_the_text_lists_the_messages_with_no_field_first():
    error = monorm.ValidationError({"address.city": "is required"}, non_field_messages=["The order is locked."])
    assert str(error) == "The order is locked.; address.city: is required"
    assert str(monorm.ValidationError()) == "validation failed"
    assert str(error.add("b", "x")) == "The order is locked.; address.city: is required; b: x"


def test_the_display_text_is_the_same_in_rust_and_the_language():
    for fields, non_field_messages, text in DISPLAY_TEXTS:
        error = monorm.ValidationError(fields, non_field_messages=non_field_messages)
        assert str(error) == text
        assert _native._test_validation_text(error) == text


def test_paths_keep_the_order_of_first_use_whatever_their_names():
    paths = ["b", "constructor", "2", "__proto__", "a", "0", "toString"]
    error = monorm.ValidationError()
    for path in paths:
        error.add(path, "x")
    error.add("constructor", "y")
    assert list(error.fields) == paths
    assert error.fields["constructor"] == ["x", "y"]
    with pytest.raises(monorm.ValidationError) as caught:
        _native._test_raise_validation_error(error)
    assert caught.value is not error
    assert list(caught.value.fields) == paths
    assert caught.value.fields["constructor"] == ["x", "y"]


def test_it_is_an_orm_error_that_the_package_exports():
    assert issubclass(monorm.ValidationError, monorm.ORMError)
    assert "ValidationError" in monorm.__all__


def test_rust_reads_the_paths_and_messages_and_raises_them_back():
    original = monorm.ValidationError({"b": "x", "lines.2.qty": ["y", "z"], "a": "w"}, non_field_messages=["m"])
    with pytest.raises(monorm.ValidationError) as caught:
        _native._test_raise_validation_error(original)
    raised = caught.value
    assert raised is not original
    assert list(raised.fields.items()) == [("b", ["x"]), ("lines.2.qty", ["y", "z"]), ("a", ["w"])]
    assert raised.non_field_messages == ["m"]
    assert str(raised) == str(original)
    other = ValueError("not a validation error")
    with pytest.raises(ValueError) as caught:
        _native._test_raise_validation_error(other)
    assert caught.value is other


def test_a_copy_keeps_the_paths_and_messages():
    error = monorm.ValidationError({"lines.2.qty": "must be positive"}, non_field_messages=["m"])
    copy = pickle.loads(pickle.dumps(error))
    assert (copy.fields, copy.non_field_messages) == (error.fields, error.non_field_messages)
    assert copy.args == error.args == (error.fields, error.non_field_messages)
    assert repr(copy) == repr(error)


class _Rejected(monorm.ValidationError):
    pass


class _Unreadable:
    """A value whose conversion runs user code that raises `error`."""

    def __init__(self, error):
        self.error = error

    def __index__(self):
        raise self.error


async def test_an_error_that_user_code_raises_during_conversion_comes_back_unchanged():
    registry = monorm.Registry()
    models = monorm.loads(SCHEMA, registry=registry)
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        error = _Rejected({"views": "is odd"}, non_field_messages=["m"])
        with pytest.raises(_Rejected) as caught:
            await models["Doc"].objects.using(db).get(models["Doc"].views == _Unreadable(error))
        assert caught.value is error
    finally:
        await db.close()
