"""A tour of the Python API on SQLite or PostgreSQL. Run from the repo root:

    python examples/blog/demo.py sqlite://:memory:
    python examples/blog/demo.py postgres://postgres:postgres@localhost/orm_test

The core section runs on every build. Each extension section runs when the native
library has that extension, and prints how to get it otherwise. Build the library with
all extensions (see docs/build-time-extensions.md, "Select feature extensions"):

    scripts/feature-check.sh all     # or the steps in that doc; then:
    PYTHONPATH=target/feature-check/all/py ORM_PROFILE=custom python examples/blog/demo.py sqlite://:memory:

The file-storage section also needs `uv pip install -e storage/python -e storage/integration/python`.
Each section creates its demo_* tables and drops them at the end.
"""

import asyncio
import shutil
import sys
import tempfile
import warnings
from collections.abc import Awaitable, Callable
from pathlib import Path
from typing import Any

import orm
from orm import func

HERE = Path(__file__).resolve().parent / "demo"


def schema(name: str, provider: str) -> str:
    return f'datasource db {{\n  provider = "{provider}"\n}}\n' + (HERE / f"{name}.prisma").read_text()


async def section(name: str, url: str, run: Callable[[orm.Database, dict[str, Any]], Awaitable[None]],
                  load: Callable[[str, orm.Registry], dict[str, Any]] | None = None) -> None:
    """Load the section's schema into its own registry, run it, and drop its tables."""
    print(f"\n== {name}")
    provider = "sqlite" if url.startswith("sqlite:") else "postgresql"
    registry = orm.Registry()
    try:
        models = load(provider, registry) if load else orm.loads(schema(name, provider), registry=registry)
    except orm.SchemaError as e:
        # An extension attribute, or the extension type `Generic`, on a build without it.
        if "not compiled into this artifact" not in str(e) and "extension" not in str(e):
            raise
        print(f"skipped: this native library has no {name} extension ({e})")
        return
    db = await orm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        await run(db, models)
    finally:
        await db.drop_tables()
        await db.close()


async def core(db: orm.Database, m: dict[str, Any]) -> None:
    User, Post, Comment, Tag, AuditLog, Role = (m[k] for k in ("User", "Post", "Comment", "Tag", "AuditLog", "Role"))

    # Writes are explicit statements: INSERT ... RETURNING gives back the stored rows.
    # `token` has @client_default(uuid()): the ORM fills it, on SQLite too.
    alice = await User.objects.using(db).insert(email="alice@example.com", name="Alice", role=Role.admin)
    bob = await User.objects.using(db).insert(email="bob@example.com", name="Bob")
    print("client default token:", len(alice.token) == 36)
    posts = await Post.objects.using(db).insert_many([
        {"author": alice, "title": f"Post {i}", "views": i, "published": i % 2 == 0} for i in range(6)
    ])
    await posts[0].comments.insert(body="first!", author=bob)
    # Upsert on the unique email.
    bob = await User.objects.using(db).insert(email="bob@example.com", name="Robert").on_conflict(User.email).do_update()
    print("upserted:", bob.name)

    # Filters follow relations; a to-many hop is an EXISTS, so no duplicate rows.
    print("authors with a published post:", [u.name for u in await User.objects.using(db).filter(User.posts.published)])
    # Eager loading: JOIN for to-one, one IN query for to-many; -column orders descending.
    c = await Comment.objects.using(db).select_related(Comment.post.author, Comment.author).get()
    print(f"{c.author.name} on {c.post.author.name}'s {c.post.title!r}")
    for u in await User.objects.using(db).prefetch_related(User.posts).order_by(-User.name):
        print(u.name, [p.title for p in u.posts.cached])
    # Explicit loaders for one reference (the reference-loading feature, on by default).
    print("loaded:", (await c.load_post()).title)

    # select(): rows of columns, aggregates, string functions and correlated subqueries.
    rows = await User.objects.using(db).select(
        func.concat(User.name, " <", User.email, ">").label("who"),
        func.count(User.posts).label("posts"),
        orm.exists(Post.objects.filter(Post.author_id == orm.outer(User.id), Post.views > 4)).label("popular"),
    ).order_by(User.name)
    print([tuple(r) for r in rows])

    # Many-to-many through PostTag.
    await posts[0].tags.add(await Tag.objects.using(db).insert(name="rust"))
    print("tagged rust:", await Post.objects.using(db).filter(Post.tags.name == "rust").count())

    # Prepared queries: build once, bind values per call.
    by_author = Post.objects.using(db).filter(Post.author_id == orm.param("author")).order_by(Post.id).limit(orm.param("n")).prepare()
    print("prepared:", [p.title for p in await by_author(author=alice.id, n=2)])

    # Cursor pagination: keyset pages with an opaque cursor.
    qs = Post.objects.using(db).order_by(-Post.views)
    page = await qs.paginate(first=4)
    print("page 1:", [p.views for p in page.items], "has next:", page.has_next)
    page = await qs.paginate(first=4, after=page.next_cursor)
    print("page 2:", [p.views for p in page.items], "has next:", page.has_next)

    # Set-based UPDATE, and a single-row update that refreshes the instance.
    await Post.objects.using(db).filter(Post.author_id == alice.id).update(views=Post.views + 1)
    async with db.transaction():
        await posts[0].update(title="Hello, world", views=Post.views + 10)
    print("views after update:", posts[0].views)  # 11: the database's value, read back with RETURNING

    # Protected writes: an application-level check, not a database permission.
    try:
        await AuditLog.objects.using(db).insert(action="outside")
    except orm.WriteProtected as e:
        print("write protected:", e)
    with orm.allow_writes(AuditLog):
        await AuditLog.objects.using(db).insert(action="inside")
    print("audit rows:", await AuditLog.objects.using(db).count())

    # The N+1 finder: one load per row in a loop is reported with its fix.
    comments = await Comment.objects.using(db).insert_many([{"post_id": p.id, "body": "+1"} for p in posts])
    comments = await Comment.objects.using(db).filter(Comment.id.in_([x.id for x in comments]))
    with warnings.catch_warnings(record=True), orm.debug.n_plus_one(threshold=3) as report:
        for x in comments:
            await x.load_post()
    print("N+1:", [(s.count, s.fix) for s in report.repeated])


async def proxy_models(db: orm.Database, m: dict[str, Any]) -> None:
    Account, ActiveAccount, Status = m["Account"], m["ActiveAccount"], m["Status"]
    await Account.objects.using(db).insert_many([
        {"id": 1, "name": "old", "status": Status.OLD, "note": "x"},
        {"id": 2, "name": "live", "status": Status.ACTIVE},
    ])
    # The proxy fills its client defaults and has no `note` field.
    created = await ActiveAccount.objects.using(db).insert(id=3)
    print("proxy insert:", created.name, created.status, "note" in ActiveAccount._meta.fields)
    print("rows through the proxy:", await ActiveAccount.objects.using(db).count(), "(one shared table)")


async def query_defaults(db: orm.Database, m: dict[str, Any]) -> None:
    Article = m["Article"]
    await Article.objects.using(db).insert_many([
        {"title": "a", "body": "long"}, {"title": "b", "body": "long"}, {"title": "hidden", "body": "x", "visible": False},
    ])
    rows = await Article.objects.using(db)
    print("default filter and order:", [r.title for r in rows])
    try:
        rows[0].body
    except orm.NotLoaded:
        print("body is selected out by default")
    print("without_defaults():", await Article.objects.using(db).without_defaults().count(), "rows")


async def model_composition(db: orm.Database, m: dict[str, Any]) -> None:
    Person, Employee, Customer = m["Person"], m["Employee"], m["Customer"]
    ann = await Employee.objects.using(db).insert(name="Ann", salary=10)   # a Person row and an Employee row
    await Customer.objects.using(db).attach(ann.id, {"points": 5})         # Ann is also a customer
    await Employee.objects.using(db).filter(Employee.name == "Ann").update(salary=12)
    person = await Person.objects.using(db).select_related(Person.employee, Person.customer).get()
    print("composed:", person.name, person.employee.salary, person.customer.points)


async def file_storage(db: orm.Database, m: dict[str, Any]) -> None:
    try:
        from orm_file_storage import FileField, Upload
        from orm_file_storage.model import install_model
        from orm_storage import LocalStorage, Registry
    except ImportError:
        print("skipped: uv pip install -e storage/python -e storage/integration/python")
        return
    Report = m["Report"]
    with tempfile.TemporaryDirectory() as directory:
        install_model(Report, {"file": FileField("file", "reports", True)}, Registry({"reports": LocalStorage("reports", directory)}))
        report = await Report.objects.using(db).insert(id=1, file=Upload(b"quarterly numbers", filename="q3.txt"))
        report = await Report.objects.using(db).get(Report.id == 1)
        content = b"".join([chunk async for chunk in report.file_open()])
        print("file field:", type(report.file).__name__, content)


def load_generic(provider: str, registry: orm.Registry) -> dict[str, Any]:
    # Identities live next to the schema; the demo copies both to a scratch directory.
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory) / "schema.prisma"
        path.write_text(schema("generic-relations", provider))
        shutil.copy(HERE / "generic-relations.identities.json", Path(directory) / "schema.identities.json")
        return orm.load(path, registry=registry)


async def generic_relations(db: orm.Database, m: dict[str, Any]) -> None:
    Photo, Video, Label, ContentType = m["Photo"], m["Video"], m["Label"], m["ContentType"]
    photo = await Photo.objects.using(db).insert(id=1, url="cat.png")
    await Video.objects.using(db).insert(id=1, title="cats")
    await Label.objects.using(db).insert_many([
        {"id": 1, "name": "cute", "target_type": ContentType.Photo, "target_id": photo.id},
        {"id": 2, "name": "long", "target_type": ContentType.Video, "target_id": 1},
    ])
    on_photos = await Label.objects.using(db).filter(Label.target_type == ContentType.Photo)
    print("labels on photos:", [x.name for x in on_photos], "ContentType.Photo =", ContentType.Photo.value)


async def main(url: str) -> None:
    await section("core", url, core)
    await section("proxy-models", url, proxy_models)
    await section("query-defaults", url, query_defaults)
    await section("model-composition", url, model_composition)
    await section("file-storage", url, file_storage)
    await section("generic-relations", url, generic_relations, load_generic)


if __name__ == "__main__":
    asyncio.run(main(sys.argv[1] if len(sys.argv) > 1 else "sqlite://:memory:"))
