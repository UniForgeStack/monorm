"""The host surface for extension language code."""

from blog.models import Post, User

from orm import Delete, ext


def test_the_hard_delete_statement_is_a_hard_delete_of_the_query_set():
    qs = User.objects.filter(User.id == 1)
    assert ext.hard_delete_statement(qs).inspect() == Delete.build(qs, hard=True).inspect()


def test_every_exported_name_exists_and_the_dead_row_query_is_gone():
    assert all(hasattr(ext, name) for name in ext.__all__)
    assert not hasattr(ext, "row_query")


def test_behavior_field_takes_the_ir_field_name():
    record = {"model": "User", "field": "created_at"}
    assert ext.behavior_field(User, record)._field.name == "created_at"
    assert ext.behavior_field(User.__new__(User), record)._field.name == "created_at"


def test_behavior_lookup_is_empty_without_records():
    assert ext.behavior_records(Post, "soft_delete") == []
    assert ext.LANGUAGE_CONTRACT == 1
