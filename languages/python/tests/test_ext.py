"""The host surface for extension language code."""

from blog.models import Post, User

from orm import Delete, ext


def test_the_hard_delete_statement_is_a_hard_delete_of_the_query_set():
    qs = User.objects.filter(User.id == 1)
    assert ext.hard_delete_statement(qs).inspect() == Delete.build(qs, hard=True).inspect()


def test_the_row_query_of_an_instance_matches_its_primary_key():
    row = User.__new__(User)
    row.__dict__["id"] = 7
    assert ext.row_query(row).inspect() == User.objects.without_defaults().filter(User.id == 7).inspect()


def test_behavior_lookup_is_empty_without_records():
    assert ext.behavior_records(Post, "soft_delete") == []
    assert ext.LANGUAGE_CONTRACT == 1
