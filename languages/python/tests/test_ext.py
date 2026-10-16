"""The host surface for extension language code."""

from blog.models import Post, User

from orm import ext


def test_the_hard_delete_statement_matches_the_query_set_method():
    qs = User.objects.filter(User.id == 1)
    assert ext.hard_delete_statement(qs).inspect() == qs.hard_delete().inspect()


def test_behavior_lookup_is_empty_without_records():
    assert ext.behavior_records(Post, "soft_delete") == []
    assert ext.LANGUAGE_CONTRACT == 1
