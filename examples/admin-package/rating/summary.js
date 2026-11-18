// A detail tab: it reads the custom route `rating/{id}/summary` and shows it on a kit card. No iframe: it is a component of the page.
import { createElement as h, useEffect, useState } from "react";
import { Badge, Card, CardContent, CardHeader, CardTitle } from "monorm-admin/ui";

export const adminMajor = 1;

export default function Summary({ id, call }) {
  const [data, setData] = useState(null);
  const [problem, setProblem] = useState(null);
  useEffect(() => {
    let live = true;
    call("GET", `rating/${id}/summary`).then((d) => live && setData(d), (e) => live && setProblem(String(e.message ?? e)));
    return () => { live = false; };
  }, [id]);
  if (problem) return h("p", { role: "alert" }, problem);
  if (!data) return h("p", { className: "muted" }, "Loading…");
  return h(
    Card,
    { className: "rating-summary" },
    h(CardHeader, null, h(CardTitle, null, data.title)),
    h(CardContent, null, h("p", null, "Stars: ", h(Badge, null, String(data.stars ?? "none"))), h("p", null, `Average of ${data.count} rated: ${data.average ?? "none"}`)),
  );
}
