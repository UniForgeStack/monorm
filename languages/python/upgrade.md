# Upgrade notes for Python

This file lists the breaking changes of each version that affect Python code.
The full list is in [CHANGELOG.md](../../CHANGELOG.md).

## 0.1.0

The project changed its name from `orm` to `monorm`.

- Install `monorm` from PyPI.
- Import `monorm` in place of `orm`.
- Run the command `monorm` in place of `orm`.

## 0.1.1

No change for Python.
The npm package name changed, and only TypeScript code is affected.

## Unreleased

`sync_permissions` takes the permission names, not models and a label.

- Before: `await sync_permissions([Post, Comment], "blog")`.
- After: `await sync_permissions(model_permission_names([Post, Comment], "blog"))`.
- Import `model_permission_names` from `monorm_auth`.
