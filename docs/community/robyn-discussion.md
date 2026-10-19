# Outreach draft: Robyn discussion

Status: **draft, not posted.** Nothing here has been sent to GitHub yet.

## The plan

Target: [sparckles/Robyn](https://github.com/sparckles/Robyn), the async Python web framework with a Rust runtime (PyO3).
It has GitHub Discussions and a [Discord](https://discord.gg/rkERZ5eNU8).

Why Robyn:

- It was one of the first projects to show that a Python framework can run on a Rust runtime through PyO3. That is the same model as MonOrm.
- Robyn already lists databases in its docs and in `robyn --create`. The scaffolds are `no-db`, `sqlite`, `postgres`, `mongo`, `sqlalchemy`, `sqlmodel` and `prisma`. The example app (`docs_src/.../example_app/modeling_routes.mdx`) uses SQLAlchemy.
- MonOrm uses Prisma syntax for its schema, so it fits next to the existing `prisma` scaffold.

Where to post: a **Discussion**, not an issue. An issue asks the maintainers to do work. A discussion is the right place for a thank-you, an introduction and an offer. Use the "Show and tell" category if it exists, otherwise "Ideas" or "General".
After a maintainer replies, open small PRs for what they accept. Do not open them before.

The steps:

1. Finish the checklist below.
2. Post the discussion (the text is in [The discussion post](#the-discussion-post)).
3. Share the link once in the Robyn Discord (the text is in [Discord message](#discord-message)).
4. If they agree, open two PRs: a `monorm` scaffold for `robyn --create`, then a docs page or an example-app section next to SQLAlchemy.
5. Answer every reply within a day. Keep the tone of a contributor, not an advertiser.

## Checklist before posting

- [ ] The MonOrm repository is public, and its README uses the name MonOrm. Today the repo and the package are still called `orm`.
- [ ] The install command works from PyPI. The name `orm` on PyPI belongs to another project, so publish under the final name (for example `monorm`) first.
- [ ] Replace each `<...>` placeholder below.
- [ ] Run the Robyn example below against the released package, on SQLite and PostgreSQL.
- [ ] Make no speed claim against SQLAlchemy. `bench/RESULTS.md` says that no current benchmark compares the Python API with Django, SQLAlchemy, Prisma or Drizzle. Add a claim only after such a benchmark is measured and recorded.
- [ ] Check the OpenDAL and ArdiQ descriptions against their current READMEs. Optionally tell their authors before posting. Do not repeat ArdiQ's benchmark numbers, which are its own measurements.

## The discussion post

**Title:** Thank you, Robyn, and an offer: MonOrm, a Rust-core async ORM, as a database option

**Body:**

---

Hi Robyn team 👋

First, thank you. Robyn showed many of us that a Python framework does not have to choose between a pleasant Python API and a fast runtime. You keep the Python surface, put the hot path in Rust, and connect the two with PyO3. That idea has inspired many projects since.

A small ecosystem of Python tools with a Rust core is now growing around the same idea:

- **[Apache OpenDAL](https://github.com/apache/opendal)**: one data-access layer for object storage, file systems and cloud services. It has a Rust core and a Python binding (`pip install opendal`).
- **[ArdiQ](https://github.com/17tayyy/ardiq)** ([site](https://ardiq.bytay.dev/)): a small task queue for Python. Its worker loop and Redis round trips run in Rust (PyO3 and tokio), outside the GIL. Tasks stay plain Python.
- **[MonOrm](<repo-url>)**: our project, and the reason for this post.

### MonOrm

MonOrm is an async ORM for Python (and TypeScript) with one Rust core. Robyn inspired us directly: we wanted the same split for the database layer.

- You write the schema once in Prisma syntax (`schema.prisma`).
- The Rust core compiles the schema, plans the SQL and runs it on PostgreSQL or SQLite (tokio-postgres). The connection pool and row decoding stay in Rust.
- Python gets generated, typed models (`mypy --strict` and pyright clean), migrations, relation filters, prefetching, transactions, locks and an N+1 finder.
- It is async from end to end, so it fits Robyn's async handlers.

A minimal Robyn app with MonOrm:

```python
# app.py
import orm
from robyn import Robyn

from models import Post, User  # generated from schema.prisma with `python -m orm generate`

app = Robyn(__file__)
db = None


@app.startup_handler
async def startup() -> None:
    global db
    db = await orm.connect("sqlite://app.db")  # or postgres://...


@app.shutdown_handler
async def shutdown() -> None:
    await db.close()


@app.post("/users")
async def create_user(request):
    body = request.json()
    user = await User.objects.insert(name=body["name"])
    return {"id": user.id, "name": user.name}


@app.get("/users")
async def users_with_published_posts():
    users = await User.objects.filter(User.posts.published).load(User.posts)
    return [
        {"name": u.name, "posts": [p.title for p in u.posts.cached]}
        for u in users
    ]


if __name__ == "__main__":
    app.start(host="0.0.0.0", port=8080)
```

### Our offer

We would like to help this ecosystem grow, and we hope Robyn will help too. If you agree, we would be glad to contribute, and to maintain:

1. a `monorm` template for `robyn --create`, next to `sqlalchemy`, `sqlmodel` and `prisma`;
2. a MonOrm section or page in the docs next to the SQLAlchemy example, for example the Gotham crime-data example app with MonOrm models;
3. a "Rust-powered Python ecosystem" list in the community resources, if you want one, with Robyn, OpenDAL, ArdiQ and others.

We will keep the examples small and in Robyn's style, and we will update them when either project changes. If you prefer something else, or nothing for now, please say so.

Thank you again for Robyn and for the path it opened.

<your name>, MonOrm (<repo-url>)

---

## Discord message

> Hi all! We just posted a thank-you and an integration offer in Discussions: <discussion-url>.
> MonOrm is an async ORM with a Rust core (PyO3), inspired by Robyn. We would like to contribute a `robyn --create` template and docs next to SQLAlchemy, if the maintainers want them. Feedback welcome 🙏

## The follow-up PRs (after a yes)

1. **Scaffold**: add `robyn/scaffold/monorm/` with `app.py`, `schema.prisma`, `requirements.txt` and `Dockerfile`, in the same shape as `robyn/scaffold/prisma/`. Add `Choice("monorm", name="MonOrm")` to the project-type prompt in `robyn/cli.py`.
2. **Docs**: add a MonOrm tab or section to `docs_src/src/pages/documentation/en/example_app/modeling_routes.mdx` (and `zh` if they want it), with the same `Crime` model written in `schema.prisma`.
3. **Our side**: add a Robyn example under `examples/robyn` in this repo, and link Robyn from the README.
