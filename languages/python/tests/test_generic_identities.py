"""Public identity/ContentType seam; full generic execution is tested separately."""

import json
import os

import pytest
import monorm
from monorm.__main__ import main as cli
from artifact_caps import has_extension


def generate(path):
    assert cli(["--schema", str(path), "identities"]) == 0


@pytest.mark.case("identities::content_type_identity_roundtrip_and_frozen_generation")
@pytest.mark.parametrize(
    "url,provider",
    [
        ("sqlite://:memory:", "sqlite"),
        pytest.param(
            os.environ.get("ORM_TEST_DATABASE_URL", ""),
            "postgresql",
            marks=pytest.mark.skipif(
                not os.environ.get("ORM_TEST_DATABASE_URL"),
                reason="set ORM_TEST_DATABASE_URL for PostgreSQL acceptance",
            ),
        ),
    ],
)
async def test_runtime_and_generated_identities_roundtrip(tmp_path, url, provider):
    schema = tmp_path / "schema.prisma"
    schema.write_text(
        f'datasource db {{\nprovider = "{provider}"\n}}\nmodel Post {{\nid Int @id\n@@map("identity08_posts")\n}}\nmodel Tag {{\nid Int @id\nkind ContentType\nobject_id Int\n@@map("identity08_tags")\n}}'
    )
    generate(schema)
    frozen = schema.with_suffix(".identities.json").read_bytes()
    registry = monorm.Registry()
    models = monorm.load(schema, registry=registry)
    assert registry.ir()["identities"] == json.loads(frozen)
    assert models["ContentType"].Post.value == 1
    assert models["ContentType"].Tag.value == 2
    db = await monorm.connect(url, registry=registry, default=False)
    try:
        await db.drop_tables()
        await db.create_tables()
        post = await models["Post"].objects.using(db).insert(id=7)
        tag = await models["Tag"].objects.using(db).insert(id=1, kind=models["ContentType"].Post, object_id=post.id)
        assert tag.kind is models["ContentType"].Post
        assert tag.object_id == 7
        # Check constraints are the core integer-enum constraint, no registry table.
        with pytest.raises(monorm.IntegrityError):
            await db.execute("INSERT INTO identity08_tags (id, kind, object_id) VALUES (2, 99, 7)")
        await post.delete()
        assert (await models["Tag"].objects.using(db).get()).object_id == 7
        # Generated Python embeds the same immutable mapping, runtime needs no file.
        out = tmp_path / "models.py"
        assert cli(["--schema", str(schema), "generate", "python", "-o", str(out)]) == 0
        payload = out.read_text().split('_SCHEMA = r"""\n', 1)[1].split('\n"""', 1)[0]
        copied = monorm.Registry()
        generated = monorm.define(payload, registry=copied)
        assert copied.ir()["identities"] == registry.ir()["identities"]
        assert generated["ContentType"].Post == models["ContentType"].Post
        assert schema.with_suffix(".identities.json").read_bytes() == frozen
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.case("identities::missing_or_conflicting_manifest_fails_during_schema_definition")
def test_missing_stale_or_conflicting_manifest_fails_before_definition(tmp_path):
    schema = tmp_path / "schema.prisma"
    schema.write_text("model Post {\nid Int @id\n}\nmodel Tag {\nid Int @id\nkind ContentType\n}")
    with pytest.raises(monorm.SchemaError, match="monorm identities"):
        monorm.load(schema, registry=monorm.Registry())
    generate(schema)
    manifest_path = schema.with_suffix(".identities.json")
    manifest = json.loads(manifest_path.read_text())
    manifest["entries"][1]["id"] = manifest["entries"][0]["id"]
    manifest_path.write_text(json.dumps(manifest))
    with pytest.raises(monorm.SchemaError, match="duplicate ContentType ID"):
        monorm.load(schema, registry=monorm.Registry())


@pytest.mark.parametrize(
    "url,provider",
    [
        ("sqlite://:memory:", "sqlite"),
        pytest.param(
            os.environ.get("ORM_TEST_DATABASE_URL", ""),
            "postgresql",
            marks=pytest.mark.skipif(
                not os.environ.get("ORM_TEST_DATABASE_URL"),
                reason="set ORM_TEST_DATABASE_URL for PostgreSQL acceptance",
            ),
        ),
    ],
)
async def test_retirement_migration_requires_reference_cleanup(tmp_path, url, provider):
    from monorm.migrations import Migrations, Migrator

    schema = tmp_path / "schema.prisma"
    header = f'datasource db {{\nprovider = "{provider}"\n}}\n'
    post = 'model Post {\nid Int @id\n@@map("identity08_migration_posts")\n}\n'
    tag = 'model Tag {\nid Int @id\nkind ContentType\nobject_id Int\n@@map("identity08_migration_tags")\n}'
    schema.write_text(header + post + tag)
    generate(schema)
    old = monorm.Registry()
    models = monorm.load(schema, registry=old)
    directory = str(tmp_path / "migrations")
    Migrations(directory, old).make("initial")
    db = await monorm.connect(url, registry=old, default=False)
    runner = Migrator(db, Migrations(directory, old))
    try:
        await db.drop_tables()
        # The migration ledger outlives drop_tables on a shared database.
        await db.execute("DROP TABLE IF EXISTS monorm_migrations")
        await runner.upgrade("1")
        await models["Tag"].objects.using(db).insert(id=1, kind=models["ContentType"].Post, object_id=9)
        schema.write_text(header + tag)
        generate(schema)
        current = monorm.Registry()
        monorm.load(schema, registry=current)
        Migrations(directory, current).make("retire_post")
        with pytest.raises(monorm.DatabaseError):
            await runner.upgrade()
        assert await models["Tag"].objects.using(db).count() == 1
        await models["Tag"].objects.using(db).all().delete()
        await runner.upgrade()
        with pytest.raises(monorm.IntegrityError):
            await db.execute("INSERT INTO identity08_migration_tags (id, kind, object_id) VALUES (2, 1, 9)")
    finally:
        await db.drop_tables()
        await db.execute("DROP TABLE IF EXISTS monorm_migrations")
        await db.close()


async def test_a_generic_relation_schema_loads_and_round_trips(tmp_path):
    from monorm import _native

    schema = tmp_path / "schema.prisma"
    schema.write_text(
        'datasource db {\nprovider = "sqlite"\n}\nmodel Post {\nid Int @id\n@@map("generic08_posts")\n}\n'
        "model Tag {\nid Int @id\ncontent_type ContentType?\nobject_id Int?\n"
        '@@generic.relation("target", type: "content_type", key: "object_id", targets: ["Post"])\n@@map("generic08_tags")\n}'
    )
    generate(schema)
    registry = monorm.Registry()
    models = monorm.load(schema, registry=registry)
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        await models["Post"].objects.using(db).insert(id=7)
        await models["Tag"].objects.using(db).insert(id=1, content_type=models["ContentType"].Post, object_id=7)
        tag = await models["Tag"].objects.using(db).get()
        assert tag.content_type is models["ContentType"].Post and tag.object_id == 7
    finally:
        await db.close()


GENERIC_HEADER = 'datasource db {\nprovider = "sqlite"\n}\nmodel Post {\nid Int @id\ntags Tag[] @generic.reverse\n@@map("generic_field_posts")\n}\nmodel Photo {\nid Int @id\n@@map("generic_field_photos")\n}\n'


@pytest.mark.case("identities::a_generic_field_creates_its_pair_its_index_and_the_reverse_field")
@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_generic_field_form_creates_the_pair_and_the_reverse(tmp_path, provider):
    url = "sqlite://:memory:" if provider == "sqlite" else os.environ.get("ORM_TEST_DATABASE_URL")
    if not url:
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL acceptance")
    from monorm import _native
    from monorm._packages import resolve_extensions

    # A bare `Generic` type gets the plain error with a package hint.
    with pytest.raises(Exception, match=r"unknown type Generic; expected a model or") as bare:
        _native.compile_schema("model Tag {\n id Int @id\n target Generic?\n}", None, resolve_extensions())
    assert "Generic belongs to the built-in generic extension and needs @generic.relation" in str(bare.value)

    schema = tmp_path / "schema.prisma"
    schema.write_text(
        GENERIC_HEADER.replace('"sqlite"', f'"{provider}"').replace(
            'tags Tag[]', 'title String\nhidden Boolean @default(false)\nshop_id Int\n'
            '@@query.defaults(filter: hidden == false)\n@@scope.by(shop: shop_id)\ntags Tag[]',
        )
        + 'model Tag {\nid Int @id\ntarget Generic? @generic.relation(targets: ["Post", "Photo"])\nnote String\n@@map("generic_field_tags")\n}'
    )
    generate(schema)
    registry = monorm.Registry()
    models = monorm.load(schema, registry=registry)
    out = tmp_path / "models.py"
    assert cli(["--schema", str(schema), "generate", "python", "-o", str(out)]) == 0
    stub = out.with_suffix(".pyi").read_text()
    assert "target: f.UnionRelation[Photo | Post," in stub
    assert "def of(self, target: type[Post]) -> _PostPath:" in stub
    assert registry.ir()["behavior"]["schema_contract_minor"] == 9
    tag = next(m for m in registry.ir()["models"] if m["name"] == "Tag")
    assert [(f["name"], f.get("nullable", False), f.get("enum")) for f in tag["fields"]] == [
        ("id", False, None),
        ("target_type", True, "ContentType"),
        ("target_id", True, None),
        ("note", False, None),
    ]
    assert tag["indexes"] == [{"columns": [{"field": "target_type"}, {"field": "target_id"}]}]
    data = registry.ir()["behavior"]["extension_data"]["monorm.generic"]
    assert data["relations"] == [
        {
            "model": "Tag",
            "name": "target",
            "type_field": "target_type",
            "key_field": "target_id",
            "targets": ["Photo", "Post"],
        }
    ]
    assert data["reverse"] == [{"model": "Post", "name": "tags", "source": "Tag", "relation": "target"}]
    db = await monorm.connect(url, registry=registry, default=False)
    try:
        await db.drop_tables()
        await db.create_tables()
        Post, Photo, Tag = (models[name] for name in ("Post", "Photo", "Tag"))
        types = models["ContentType"]
        with monorm.scope(shop=1):
            await Post.objects.using(db).insert_many([
                {"id": 7, "title": "visible"}, {"id": 8, "title": "hidden", "hidden": True},
            ])
        with monorm.scope(shop=2):
            await Post.objects.using(db).insert(id=9, title="foreign")
        photo = await Photo.objects.using(db).insert(id=7)
        tags = Tag.objects.using(db)
        await tags.insert_many([
            {"id": 1, "target_type": types.Post, "target_id": 7, "note": "post"},
            {"id": 2, "target_type": types.Photo, "target_id": 7, "note": "photo"},
            {"id": 3, "target_type": types.Post, "target_id": 8, "note": "hidden"},
            {"id": 4, "target_type": types.Post, "target_id": 9, "note": "foreign"},
            {"id": 5, "note": "empty"},
        ])
        with monorm.scope(shop=1):
            statements = []
            stop = db.on_query(lambda event: statements.append(event.sql))
            try:
                rows = await tags.load(Tag.target).order_by(Tag.id).all()
            finally:
                stop()
            assert len([sql for sql in statements if sql.lstrip().upper().startswith("SELECT")]) == 3
            assert [type(row.target) for row in rows] == [Post, Photo, type(None), type(None), type(None)]
            row = rows[0]
            row.target = photo
            assert row.target is photo and row.target_type is types.Photo and row.target_id == 7

            typed = tags.filter(Tag.target.of(Post).title == "visible")
            assert "EXISTS" in typed.sql()
            assert [row.id for row in await typed.all()] == [1]
            assert not await tags.filter(Tag.target.of(Post).title.in_(["hidden", "foreign"])).all()
            with pytest.raises(monorm.QueryError):
                await tags.order_by(Tag.target.of(Post).title).all()

            posts = Post.objects.using(db)
            post = await posts.load(Post.tags).get()
            assert [tag.id for tag in post.tags.cached] == [1]
            assert [tag.id for tag in await post.tags.filter(Tag.note == "post").all()] == [1]
            assert await post.tags.count() == 1
            assert [p.id for p in await posts.filter(Post.tags.note == "post").all()] == [7]
            assert not await posts.filter(Post.tags.note == "photo").all()
            partial = await tags.load(Tag.note).filter(Tag.id == 1).get()
            await partial.load(Tag.target)
            assert partial.target.id == 7

        # A table from before the pair constraint can contain a registered type
        # which this relation does not allow.
        await db.execute("DROP TABLE generic_field_tags")
        await db.execute("CREATE TABLE generic_field_tags (id INTEGER PRIMARY KEY, target_type INTEGER, target_id INTEGER, note TEXT)")
        await db.execute(f"INSERT INTO generic_field_tags VALUES (6, {types.Tag.value}, 7, 'invalid')")
        with monorm.scope(shop=1), pytest.raises(monorm.DatabaseError, match=r"^Tag\.target: unknown relation type \d+$"):
            await tags.load(Tag.target).all()
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.case("identities::a_union_relation_refuses_what_is_not_a_target_and_what_is_not_whole")
async def test_a_union_relation_refuses_what_is_not_a_target_and_what_is_not_whole(tmp_path):
    schema = tmp_path / "schema.prisma"
    schema.write_text(
        GENERIC_HEADER.replace("generic_field_posts", "generic_err_posts").replace("generic_field_photos", "generic_err_photos")
        + 'model Tag {\nid Int @id\ntarget Generic? @generic.relation(targets: ["Post", "Photo"])\nnote String?\n@@map("generic_err_tags")\n}'
    )
    generate(schema)
    registry = monorm.Registry()
    models = monorm.load(schema, registry=registry)
    Post, Tag = models["Post"], models["Tag"]
    types = models["ContentType"]
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        post = await Post.objects.using(db).insert(id=7)
        tags = Tag.objects.using(db)
        await tags.insert(id=1, target_type=types.Post, target_id=7)
        tag = await tags.filter(Tag.id == 1).get()

        with pytest.raises(monorm.NotLoaded, match=r"Tag\.target is not loaded"):
            tag.target
        for value in (tag, "x", None):
            expected = r"Tag\.target: Tag is not a relation target" if value is tag else r"Tag\.target: expected a relation target instance"
            with pytest.raises(TypeError, match=expected):
                tag.target = value
        with pytest.raises(TypeError, match=r"Tag\.target: Tag is not a relation target"):
            Tag.target.of(Tag)
        for item in (Tag.target.of(Post), Tag.target.of(Post).id):
            with pytest.raises(monorm.QueryError, match=r"^load\(\) needs the whole union relation Tag\.target$"):
                tags.load(item)

        created = await post.tags.insert(id=2, note="through the reverse")
        assert (created.target_type, created.target_id) == (types.Post, 7)
        assert [(t.id, t.target_type) for t in await Tag.objects.using(db).filter(Tag.target_id == 7).order_by(Tag.id).all()] == [
            (1, types.Post), (2, types.Post),
        ]
    finally:
        await db.close()


@pytest.mark.case("identities::a_union_relation_loads_targets_with_string_keys")
async def test_a_union_relation_loads_targets_with_string_keys(tmp_path):
    schema = tmp_path / "schema.prisma"
    schema.write_text(
        'datasource db {\nprovider = "sqlite"\n}\n'
        'model Post {\nid String @id\ntags Tag[] @generic.reverse\n@@map("generic_str_posts")\n}\n'
        'model Photo {\nid String @id\n@@map("generic_str_photos")\n}\n'
        'model Tag {\nid Int @id\ntarget Generic? @generic.relation(targets: ["Post", "Photo"])\n@@map("generic_str_tags")\n}'
    )
    generate(schema)
    registry = monorm.Registry()
    models = monorm.load(schema, registry=registry)
    Post, Photo, Tag = (models[name] for name in ("Post", "Photo", "Tag"))
    types = models["ContentType"]
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        await Post.objects.using(db).insert(id="p-1")
        await Photo.objects.using(db).insert(id="p-1")
        tags = Tag.objects.using(db)
        await tags.insert_many([
            {"id": 1, "target_type": types.Post, "target_id": "p-1"},
            {"id": 2, "target_type": types.Photo, "target_id": "p-1"},
        ])
        rows = await tags.load(Tag.target).order_by(Tag.id).all()
        assert [type(row.target) for row in rows] == [Post, Photo]
        post = await Post.objects.using(db).load(Post.tags).get()
        assert [tag.id for tag in post.tags.cached] == [1]
    finally:
        await db.close()


@pytest.mark.case("identities::moving_to_the_generic_field_form_needs_no_migration")
def test_moving_to_the_generic_field_form_needs_no_migration(tmp_path):
    from monorm import _native
    from monorm.migrations import Migrations

    schema = tmp_path / "schema.prisma"
    explicit = (
        "model Tag {\nid Int @id\ntarget_type ContentType?\ntarget_id Int?\nkind ContentType\nobject_id Int\n"
        '@@generic.relation("target", type: "target_type", key: "target_id", targets: ["Post", "Photo"])\n'
        '@@generic.relation("owner", type: "kind", key: "object_id", targets: ["Post"])\n'
        '@@index([target_type, target_id])\n@@map("generic_field_tags")\n}'
    )
    field = (
        'model Tag {\nid Int @id\ntarget Generic? @generic.relation(targets: ["Post", "Photo"])\nkind ContentType\nobject_id Int\n'
        'owner Generic @generic.relation(type: "kind", key: "object_id", targets: ["Post"], index: false)\n@@map("generic_field_tags")\n}'
    )
    directory = str(tmp_path / "migrations")
    for i, model in enumerate([explicit, field]):
        schema.write_text(
            GENERIC_HEADER.replace("tags Tag[] @generic.reverse", 'tags Tag[] @generic.reverse(relation: "target")')
            + model
        )
        generate(schema)
        registry = monorm.Registry()
        monorm.load(schema, registry=registry)
        made = Migrations(directory, registry).make("initial")
        assert (made == []) == (i == 1)


async def test_a_proxy_that_keeps_a_generic_field_fails_and_one_without_it_loads(tmp_path):
    if not has_extension("monorm.generic", "monorm.proxy"):
        pytest.skip("needs the monorm.generic and monorm.proxy extensions")
    schema = tmp_path / "schema.prisma"
    tag = 'model Tag {\nid Int @id\ntarget Generic? @generic.relation(targets: ["Post"])\nnote String\n@@map("generic_field_tags")\n}\n'
    for fields in ['include: ["id", "note"]', 'exclude: ["target"]']:
        schema.write_text(GENERIC_HEADER + tag + f"model NotedTag {{\n@@proxy.of(Tag)\n@@proxy.fields({fields})\n}}")
        generate(schema)
        registry = monorm.Registry()
        models = monorm.load(schema, registry=registry)
        db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
        try:
            await db.create_tables()
            await (
                models["Tag"]
                .objects.using(db)
                .insert(id=1, target_type=models["ContentType"].Post, target_id=7, note="n")
            )
            assert [(t.id, t.note) for t in await models["NotedTag"].objects.using(db)] == [(1, "n")]
        finally:
            await db.close()
    # The identity manifest of the same models stays; a full proxy keeps the Generic field.
    schema.write_text(GENERIC_HEADER + tag + "model NotedTag {\n@@proxy.of(Tag)\n}")
    with pytest.raises(monorm.SchemaError, match="Tag.target: NotedTag keeps the Generic field"):
        monorm.load(schema, registry=monorm.Registry())


@pytest.mark.case("identities::models_generated_for_contract_2_ask_for_regeneration")
@pytest.mark.parametrize(
    "behavior",
    ['{"schema_contract": 2, "generic_relations": [{"model": "Tag"}]}', '{"schema_contract": 2}'],
)
def test_models_generated_for_contract_2_ask_for_regeneration(behavior):
    from monorm import _native

    with pytest.raises(_native.SchemaError, match="regenerate"):
        _native.prepare_schema('{"models": [], "behavior": ' + behavior + "}")
