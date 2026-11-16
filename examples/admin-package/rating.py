"""An admin extension package for Python: a stars widget, a summary tab, a route and an action, plugged in with one call.

    from rating import install
    install(admin, Movie)

`Movie` is a model with an integer `stars` field. The package lives in the `rating/` directory next to this file.
The same package works in a TypeScript project: see `rating.mjs`.
"""

from pathlib import Path

from monorm import func
from monorm.admin import Reply, action, message

HERE = Path(__file__).parent / "rating"


def install(admin, model):
    admin.package("rating", HERE, widgets={"stars": "stars.js"}, tabs={"summary": "summary.js"}, styles=["rating.css"])

    async def summary(ctx, request):
        key = request.params["id"]
        mine = await model.objects.using(ctx.database).filter(model.id == int(key)).first() if key.isdigit() else None
        if mine is None:
            return Reply({"error": "no such record"}, status=404)
        # One aggregate query: the route never loads the rows.
        totals = await model.objects.using(ctx.database).select(func.count(model.stars).label("rated"), func.sum(model.stars).label("total")).first()
        rated = totals.rated
        return {"title": mine.title, "stars": mine.stars, "count": rated, "average": round(float(totals.total) / rated, 2) if rated else None}

    admin.route("GET", "rating/{id}/summary", summary)

    @action(name="reset_stars", label="Reset stars", placement="detail", confirm="Remove the stars?")
    async def reset_stars(ctx, qs):
        await qs.update(stars=None)
        return message("Stars removed", "success")

    admin.register(model, {"widgets": {"stars": "rating.stars"}, "detail_tabs": ["rating.summary"], "actions": [reset_stars]})
