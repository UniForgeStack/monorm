# monorm

The contracts between the monorm host and an extension: the extension manifest (`monorm-extension.toml`), the hook and ABI types, and the schema IR.
An extension crate depends on this crate and on nothing else of the host.

This crate has no binary: `cargo install monorm` installs nothing.
The `monorm` command comes with the Python package (`pip install monorm`) and the npm package (`npm install monorm`).

The public API is not stable before 1.0.
Licensed under MIT.
