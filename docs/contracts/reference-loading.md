# Reference loading: the adapter contract

The user guide is [loading-instances.md](../guides/loading-instances.md).
This page is for the people who build the bindings and the adapters.

## The Cargo feature

The `reference-loading` Cargo feature enables the to-one cache adapter and the shared in-progress queries for simple reference loads.
The Python and Node bindings and the CLI enable it ([optional-packaging.md](optional-packaging.md)).
A build without the feature supports instance `.load(...)` through the ordinary relation prefetch planner, but does not import the adapter.
Generated modules still record their required capabilities.

## Hidden helper keys

An adapter reads the hidden helper keys: it looks first at the public attribute, then at the hidden values.
Ordinary attribute access does not show the hidden values.

## Generic references

An adapter for generic references can use the internal function that shares concurrent loads.
That function does not route generic references.
