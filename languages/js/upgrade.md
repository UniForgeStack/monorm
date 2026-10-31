# Upgrade notes for TypeScript

This file lists the breaking changes of each version that affect TypeScript code.
The full list is in [CHANGELOG.md](../../CHANGELOG.md).

## 0.1.0

The project changed its name from `orm` to `monorm`.

- Import `monorm` in place of `orm`.
- Run the command `monorm` in place of `orm`.

## 0.1.1

The npm package is `@ufstack/monorm`, because npm rejected the name `monorm`.

- Install it under the alias `monorm`: `npm install monorm@npm:@ufstack/monorm`.
- Imports stay `monorm`.
- A plain `npm install @ufstack/monorm` installs, but the imports fail.
