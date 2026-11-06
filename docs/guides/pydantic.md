# Pydantic and other serializers (Python)

<!-- docs-test: setup=blog-lite -->

monorm does not generate Pydantic classes, and a model does not depend on Pydantic.
You write the response schema, and Pydantic reads a monorm instance through its attributes.
Other serializers that read attributes work the same way.

## Read an instance

Set `from_attributes=True` and call `model_validate`:

```python run
import datetime
from pydantic import BaseModel, ConfigDict, TypeAdapter

class UserOut(BaseModel):
    model_config = ConfigDict(from_attributes=True)
    id: int
    email: str
    name: str
    created_at: datetime.datetime

user = await User.objects.get(User.id == 1)
UserOut.model_validate(user)

users = await User.objects.filter(User.name == "A")
TypeAdapter(list[UserOut]).validate_python(users, from_attributes=True)
```

## Partial instances

A field that the query did not load raises `NotLoaded`, which is an `AttributeError`.
Pydantic reads it as a missing attribute:

- A schema field with a default takes the default.
- A schema field without a default gives a `ValidationError` with `Field required`.

Load the fields that the schema needs:

```python run
class UserName(BaseModel):
    model_config = ConfigDict(from_attributes=True)
    id: int
    name: str

user = await User.objects.load(User.id, User.name).get(User.id == 1)
UserName.model_validate(user)
```

## Relations

Load a relation before you validate.
A to-one relation (`Post.author`) reads like a field:

```python run
class AuthorOut(BaseModel):
    model_config = ConfigDict(from_attributes=True)
    name: str

class PostOut(BaseModel):
    model_config = ConfigDict(from_attributes=True)
    id: int
    title: str
    author: AuthorOut

post = await Post.objects.load(Post.author).get(Post.id == 1)
PostOut.model_validate(post)
```

A to-many relation (`User.posts`) is a `RelatedSet`, which is a query set, not a list.
Its loaded rows are in `.cached`.
Give the schema field a `BeforeValidator` that reads `.cached`:

```python run
from typing import Annotated, TypeVar
from pydantic import BeforeValidator

T = TypeVar("T")
Loaded = Annotated[list[T], BeforeValidator(lambda rows: rows.cached)]

class PostTitle(BaseModel):
    model_config = ConfigDict(from_attributes=True)
    id: int
    title: str

class UserWithPosts(BaseModel):
    model_config = ConfigDict(from_attributes=True)
    id: int
    name: str
    posts: Loaded[PostTitle]

user = await User.objects.load(User.posts.objects.load(Post.id, Post.title)).get(User.id == 1)
UserWithPosts.model_validate(user).model_dump_json()
```

When the relation is not loaded, `.cached` raises `NotLoaded` (`User.posts was not prefetched`).
`model_validate` does not catch it.

Do not declare a to-many field as a plain `list[...]`.
Pydantic then raises a `ValidationError` at once (`Input should be a valid list`), because a query set raises `TypeError` when Python iterates it: it runs on the database, so use `async for`, `await`, or `.cached`.

## FastAPI

A FastAPI route with `response_model=UserOut` validates the returned instance the same way, because `UserOut` sets `from_attributes=True`.
Load the fields and relations of the schema in the route.
Attribute access does no I/O, so FastAPI cannot load them later.
A missing field gives a response validation error, and the client gets status 500.
