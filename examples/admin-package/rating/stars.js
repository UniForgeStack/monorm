// A widget: five stars for an integer field. The contract is `adminMajor` and a default React component.
// The bare imports work because the admin page has an import map: `react` is the admin's own React, `monorm-admin/ui` is its kit.
import { createElement as h } from "react";
import { Button, Star } from "monorm-admin/ui";

export const adminMajor = 1;

export default function Stars({ id, value, onChange, onError, readOnly, disabled, errors }) {
  const set = (n) => {
    onError(null);
    onChange(n === value ? null : n);
  };
  return h(
    "div",
    { className: "rating-stars", role: "group", "aria-label": "Stars" },
    [1, 2, 3, 4, 5].map((n) =>
      h(
        Button,
        { key: n, id: n === 1 ? id : undefined, variant: "ghost", size: "icon", className: n <= (value ?? 0) ? "rating-on" : "rating-off", "aria-pressed": n <= (value ?? 0), "aria-label": `${n} of 5`, disabled: readOnly || disabled, onClick: () => set(n) },
        h(Star, { fill: n <= (value ?? 0) ? "currentColor" : "none" }),
      ),
    ),
    errors.map((m) => h("p", { key: m, role: "note" }, m)),
  );
}
