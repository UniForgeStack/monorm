// An admin extension package for TypeScript and JavaScript projects: the same stars widget, summary tab, route and action as rating.py.
//
//     import { install } from "./rating.mjs";
//     install(admin, Movie);
//
// `Movie` is a model with an integer `stars` field. The package lives in the `rating/` directory next to this file.
import { fileURLToPath } from "node:url";

import { func } from "monorm";
import { Reply, action, message } from "monorm/admin";

const HERE = fileURLToPath(new URL("./rating/", import.meta.url));

export function install(admin, model) {
  admin.package("rating", HERE, { widgets: { stars: "stars.js" }, tabs: { summary: "summary.js" }, styles: ["rating.css"] });

  admin.route("GET", "rating/{id}/summary", async (ctx, request) => {
    const key = request.params.id;
    const mine = /^\d+$/.test(key) ? await model.objects.using(ctx.database).filter(model.id.eq(Number(key))).first() : null;
    if (mine === null) return new Reply({ error: "no such record" }, { status: 404 });
    // One aggregate query: the route never loads the rows.
    const totals = await model.objects.using(ctx.database).select({ rated: func.count(model.stars), total: func.sum(model.stars) }).first();
    const rated = Number(totals.rated);
    return { title: mine.title, stars: mine.stars, count: rated, average: rated > 0 ? Math.round((Number(totals.total) / rated) * 100) / 100 : null };
  });

  const resetStars = action(
    async (_ctx, qs) => {
      await qs.update({ stars: null });
      return message("Stars removed", "success");
    },
    { name: "reset_stars", label: "Reset stars", placement: "detail", confirm: "Remove the stars?" },
  );

  admin.register(model, { widgets: { stars: "rating.stars" }, detailTabs: ["rating.summary"], actions: [resetStars] });
}
