"""The host surface for extension language code."""

from blog.models import Post, User

from monorm import Delete, ext


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
    assert ext.behavior_records(Post, "delete_rewrites") == []
    assert ext.LANGUAGE_CONTRACT == 1


def test_behavior_value_is_none_without_a_record():
    assert ext.behavior_value("model_overrides") is None


async def test_database_of_gives_the_database_that_loaded_the_instance(db):
    [ann] = await User.objects.using(db).insert_many([{"email": "ext-db-of@example.com", "name": "Ann"}]).returning()
    try:
        loaded = await User.objects.using(db).get(User.id == ann.id)
        assert ext.database_of(loaded) is db
        assert ext.database_of(User.__new__(User)) is None
    finally:
        await User.objects.using(db).filter(User.id == ann.id).delete()


def test_canonical_hash_ignores_key_order_and_matches_fixed_vectors():
    assert ext.canonical_hash({}) == "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
    assert ext.canonical_hash({"b": [1, 2], "a": "x"}) == "721ef82f2d6c0997bffb7a8ab3f40f8fb45b0b52ce2af3afa6b0f05efbdc317f"
    assert ext.canonical_hash({"a": "x", "b": [1, 2]}) == ext.canonical_hash({"b": [1, 2], "a": "x"})


def test_json_diff_lists_changed_added_and_removed_keys_with_a_null_for_a_missing_side():
    old = {"a": 1, "b": 2, "c": 3}
    new = {"a": 1, "b": 5, "d": 4}
    assert ext.json_diff(old, new) == {
        "b": {"old": 2, "new": 5},
        "c": {"old": 3, "new": None},
        "d": {"old": None, "new": 4},
    }
    assert ext.json_diff(old, old) == {}
