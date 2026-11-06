"""A sync loop over a query set raises `TypeError` at once; it never loops through `__getitem__`."""

import pytest
from blog.models import Post, User


MESSAGE = "async for"


def sets(user):
    return {
        "query set": Post.objects,
        "related set": user.posts,
        "select": Post.objects.select(Post.title),
    }


@pytest.fixture
async def user(clean):
    return await User.objects.insert(email="a@example.com", name="Alice")


@pytest.mark.parametrize("kind", ["query set", "related set", "select"])
async def test_sync_iteration_raises_type_error(user, kind):
    qs = sets(user)[kind]
    with pytest.raises(TypeError, match=MESSAGE):
        list(qs)
    with pytest.raises(TypeError, match=MESSAGE):
        for _ in qs:
            pass
    with pytest.raises(TypeError, match=MESSAGE):
        tuple(qs)
    with pytest.raises(TypeError, match=MESSAGE):
        first, *rest = qs
    with pytest.raises(TypeError, match=r"\.cached"):
        object() in qs


@pytest.mark.parametrize("kind", ["query set", "related set", "select"])
async def test_async_iteration_await_and_slices_still_work(user, kind):
    await Post.objects.insert(author=user, title="p1", body="")
    qs = sets(user)[kind]
    assert len([row async for row in qs]) == len(await qs)
    assert len(await qs[0:1]) == 1
    assert await qs[0] is not None


async def test_pydantic_from_attributes_raises_at_once(user):
    pydantic = pytest.importorskip("pydantic")

    class PostOut(pydantic.BaseModel):
        model_config = pydantic.ConfigDict(from_attributes=True)
        title: str

    class UserOut(pydantic.BaseModel):
        model_config = pydantic.ConfigDict(from_attributes=True)
        posts: list[PostOut]

    # Pydantic swallows the TypeError of iter() and reports `list_type`; before the fix it never returned.
    with pytest.raises(pydantic.ValidationError, match="valid list"):
        UserOut.model_validate(user)
