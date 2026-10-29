# Outreach draft: Robyn discussion

Status: **draft, not posted.** Nothing here has been sent to GitHub yet.

## The plan

Target: [sparckles/Robyn](https://github.com/sparckles/Robyn), the async Python web framework with a Rust runtime (PyO3).
It has GitHub Discussions and a [Discord](https://discord.gg/rkERZ5eNU8).

The message has three parts:

1. **Thank you.** Robyn was one of the first projects to show that a Python framework can run on a Rust runtime through PyO3. MonOrm follows the same model.
2. **A Rust-powered Python ecosystem that we build together.** Robyn, MonOrm, Apache OpenDAL and ArdiQ each cover one layer of a backend (web, database, storage, background jobs). Mention OpenDAL and ArdiQ to show that an ecosystem is forming, without any integration claim. The ask is to bring these projects together and grow them as one ecosystem.
3. **A replacement for Robyn's Prisma template.** `robyn --create` offers a `prisma` template built on Prisma Client Python. That client has not been maintained since March 2025. Its maintainer gave two reasons: a lack of time, and Prisma rewriting its core from Rust to TypeScript ([RobertCraigie/prisma-client-py#1073](https://github.com/RobertCraigie/prisma-client-py/issues/1073)). MonOrm reads the same `schema.prisma` language and keeps a Rust core, so it can take over that template, next to SQLAlchemy.

Where to post: a **Discussion**, not an issue. An issue asks the maintainers to do work. A discussion fits a thank-you, an introduction and an offer. Use the "Show and tell" category if it exists, otherwise "Ideas" or "General".
After a maintainer replies, open small PRs for what they accept. Do not open them before.

The steps:

1. Finish the checklist below.
2. Post the discussion (the text is in [The discussion post](#the-discussion-post)).
3. Share the link once in the Robyn Discord (the text is in [Discord message](#discord-message)).
4. If they agree, open the PRs in [The follow-up PRs](#the-follow-up-prs-after-a-yes).
5. Optionally invite the OpenDAL and ArdiQ maintainers to the thread, so the ecosystem idea has more than one voice.
6. Answer every reply within a day. Keep the tone of a contributor, not an advertiser.

### How to say "successor of Prisma"

The post calls MonOrm a Python home for Prisma's schema idea. It does not call MonOrm Prisma's official successor:

- Prisma (the company) never shipped the Python client. Prisma Client Python was a community project. Prisma itself still exists for TypeScript, so "successor" reads as a claim about Prisma.
- MonOrm is independent of Prisma. It reuses the schema syntax and the editor extension, and runs none of Prisma's code ([docs/schema/prisma-syntax.md](../schema/prisma-syntax.md)). "Prisma" is a trademark. Wording that suggests an affiliation can draw a correction in a public thread.
- The schema language is shared, but MonOrm is not a drop-in replacement. Robyn's template schema loads with one change: `@default(cuid())` becomes `@default(uuid())`. The query API is MonOrm's own, not Prisma Client's.

Safe wording: "carries Prisma's schema-first idea forward for Python", "the same `schema.prisma` language", "a path for Prisma Client Python users".

## Checklist before posting

- [ ] The MonOrm repository is public, and its README uses the name MonOrm. The repo and the packages are named `monorm`.
- [ ] The install command `pip install monorm` works from PyPI. The name `orm` on PyPI belongs to another project, so the package is published as `monorm`.
- [ ] Replace each `<...>` placeholder below.
- [ ] Run the Robyn example below against the released package, on SQLite and PostgreSQL. (It runs today against a local build with `monorm.loads`: `POST /users` and `GET /users` return the expected JSON.)
- [ ] Write a short migration note for Prisma Client Python users (schema changes such as `cuid()`, the query API side by side) and link it from the post.
- [ ] Cite speed only from the [2026-10-28 run](../../bench/RESULTS.md#speed-up-summary) (Python against Django and SQLAlchemy, TypeScript against Prisma and Drizzle; one run on one cloud VM), with its date, machine and the cases where `monorm` is not faster.
- [ ] Check the OpenDAL and ArdiQ descriptions against their current READMEs. Do not repeat ArdiQ's benchmark numbers, which are its own measurements.

## The discussion post

**Title:** Thank you, Robyn: let's build the Rust-powered Python ecosystem together (and a MonOrm offer)

**Body:**

---

Hi Robyn team 👋

First, thank you. Robyn showed many of us that a Python framework does not have to choose between a pleasant Python API and a fast runtime. You keep the Python surface, put the hot path in Rust, and connect the two with PyO3. That idea has inspired many projects since, including ours.

### A Rust-powered Python ecosystem

Projects built on the same idea now cover most layers of a Python backend:

| Layer | Project | What it does |
|---|---|---|
| Web | **[Robyn](https://github.com/sparckles/Robyn)** | Async web framework with a Rust runtime |
| Database | **[MonOrm](<repo-url>)** | Async ORM with a Rust core, schema in `schema.prisma` |
| Storage | **[Apache OpenDAL](https://github.com/apache/opendal)** | One data-access layer for S3, GCS, Azure, local files and many more services (`pip install opendal`) |
| Background jobs | **[ArdiQ](https://github.com/17tayyy/ardiq)** | Task queue whose worker loop and Redis round trips run in Rust (PyO3 and tokio), outside the GIL |

Each project is useful alone. Together, they make a full stack where Python is the API and Rust does the heavy work in every layer. Most of these projects grew separately. We think the ecosystem grows faster if we bring them together and build it as one: link each other's docs, share examples, and point users to each other.

### MonOrm

MonOrm is an async ORM for Python (and TypeScript) with one Rust core. Robyn inspired us directly: we wanted the same split for the database layer.

- You write the schema once in Prisma syntax (`schema.prisma`). Prisma's VS Code extension gives highlighting and formatting.
- The Rust core compiles the schema, plans the SQL and runs it on PostgreSQL or SQLite. The connection pool and row decoding stay in Rust.
- Python gets generated, typed models (`mypy --strict` and pyright clean), migrations, relation filters, prefetching, transactions, locks and an N+1 finder.
- It is async from end to end, so it fits Robyn's async handlers.

### Prisma's schema idea, kept alive for Python

`robyn --create` offers a `prisma` template that uses Prisma Client Python. That client has not been maintained since March 2025: its maintainer named a lack of time and Prisma's rewrite of its core from Rust to TypeScript ([RobertCraigie/prisma-client-py#1073](https://github.com/RobertCraigie/prisma-client-py/issues/1073)).

MonOrm carries the same schema-first idea forward for Python, on a Rust core. It reads the same `schema.prisma` language. The schema of Robyn's `prisma` template loads with one change (`@default(cuid())` becomes `@default(uuid())`). It does not need Node.js or a separate engine binary. MonOrm is an independent project, not affiliated with Prisma.

A minimal Robyn app with MonOrm:

```python
# app.py
import monorm
from robyn import Robyn

from models import Post, User  # generated from schema.prisma with `python -m monorm generate`

app = Robyn(__file__)
db = None


@app.startup_handler
async def startup() -> None:
    global db
    db = await monorm.connect("sqlite://app.db")  # or postgres://...


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

1. a `monorm` template for `robyn --create`, next to `sqlalchemy` and `sqlmodel`, as a maintained option for users of the `prisma` template, and a short migration note for those users;
2. a MonOrm section or page in the docs next to the SQLAlchemy example, for example the Gotham crime-data example app with MonOrm models;
3. a "Rust-powered Python ecosystem" page in the community resources, if you want one, that lists Robyn, MonOrm, OpenDAL, ArdiQ and other projects.

We will keep the examples small and in Robyn's style, and we will update them when either project changes. If you prefer something else, or nothing for now, please say so. Other maintainers of Rust-powered Python projects are welcome in this thread too.

Thank you again for Robyn and for the path it opened.

<your name>, MonOrm (<repo-url>)

---

## Discord message

> Hi all! We just posted a thank-you and an ecosystem idea in Discussions: <discussion-url>.
> Robyn, MonOrm, OpenDAL and ArdiQ each cover one layer of a Python backend with a Rust core (PyO3). We'd love to build that ecosystem together.
> MonOrm reads `schema.prisma`, so we offered a maintained `robyn --create` template for users of the `prisma` template (Prisma Client Python is no longer maintained), plus docs next to SQLAlchemy. Feedback welcome 🙏

## The follow-up PRs (after a yes)

1. **Template**: add `robyn/scaffold/monorm/` with `app.py`, `schema.prisma`, `requirements.txt` and `Dockerfile`, in the same shape as `robyn/scaffold/prisma/`. Add `Choice("monorm", name="MonOrm")` to the project-type prompt in `robyn/cli.py`. Whether the `prisma` template stays, gets a deprecation note or is replaced is the maintainers' decision.
2. **Docs**: add a MonOrm tab or section to `docs_src/src/pages/documentation/en/example_app/modeling_routes.mdx` (and `zh` if they want it), with the same `Crime` model written in `schema.prisma`.
3. **Our side**: add `examples/robyn` to this repo, a migration note for Prisma Client Python users, and links to Robyn, OpenDAL and ArdiQ in the README.
