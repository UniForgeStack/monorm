// A widget: five stars for an integer field. The contract is `adminMajor` and a default React component.
// The bare `import "react"` works because the admin page has an import map that points at its own React.
import { createElement as h } from "react";

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
      h("button", { key: n, type: "button", id: n === 1 ? id : undefined, className: n <= (value ?? 0) ? "rating-on" : "rating-off", "aria-pressed": n <= (value ?? 0), disabled: readOnly || disabled, onClick: () => set(n) }, String(n)),
    ),
    errors.map((m) => h("p", { key: m, role: "note" }, m)),
  );
}
