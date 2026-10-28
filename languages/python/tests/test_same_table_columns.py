"""A query accepts a column of any model on the same table and resolves it by name."""

import pytest

import monorm

def model(name: str, table: str, *fields: str) -> dict:
    cols = [{"name": "id", "column": "id", "type": "int", "primary_key": True}]
    cols += [{"name": f, "column": f, "type": "bool" if f == "published" else "string"} for f in fields]
    return {"name": name, "table": table, "fields": cols}


# `define` builds models that share a table without an extension, as a proxy does.
SCHEMA = {
    "dialect": "sqlite",
    "models": [model("Article", "articles", "title", "published"), model("Slim", "articles", "title"), model("Other", "others", "title")],
}


@pytest.fixture(scope="module")
def models():
    return monorm.define(SCHEMA, registry=monorm.Registry())


def test_a_column_of_a_model_on_the_same_table_resolves_on_the_query_model(models):
    Article, Slim = models["Article"], models["Slim"]
    assert Slim.objects.filter(Article.title == "a").sql() == Slim.objects.filter(Slim.title == "a").sql()


def test_a_field_the_query_model_lacks_is_the_normal_no_field_error(models):
    Article, Slim = models["Article"], models["Slim"]
    with pytest.raises(LookupError, match="Slim has no field 'published'"):
        Slim.objects.filter(Article.published == True).sql()  # noqa: E712


def test_a_column_of_a_model_on_another_table_is_still_rejected(models):
    with pytest.raises(ValueError, match="belongs to Article"):
        models["Other"].objects.filter(models["Article"].title == "a").sql()
