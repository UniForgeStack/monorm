# Shared behavioral conformance for language and database adapters

Status: proposed implementation, not implemented behavior.

## Problem Statement

Maintainers need to know that each language adapter exposes the same ORM behavior across supported databases and feature combinations.
Today, Python and TypeScript have separate behavioral suites, while Rust has core and engine tests and a proposed public client.
Equivalent tests can drift, and a new adapter author has no reusable suite that defines compatibility independently of this checkout.

The project should demonstrate that another repository can add language or database support, verify its behavior against a shared contract, and report precisely which capabilities it supports.
Moving test files alone does not establish that boundary.

## Solution

Provide a versioned collection of shared behavioral scenarios, common schemas, expected outcomes, and a runner protocol.
Each language adapter supplies a thin runner that executes the scenarios through its public ORM API.
Expected behavior is defined once and reused across adapters and supported databases.

Use Python, TypeScript, and Rust as the three target language adapters.
Python and TypeScript can adopt the suite first.
Rust joins as its public client becomes available; engine-level Rust execution is useful supplementary coverage but does not establish public Rust client conformance.
Node and Bun are separate runtime configurations of the TypeScript adapter.

A scenario can insert two users, filter by name, update one, fetch and verify the stored value, delete it, and verify the remaining count.
Each runner translates those operations into its language's public API and checks the shared expected results.
Language-specific typing, error classes, and lifecycle behavior remain in adapter-owned tests.

An external repository can obtain a pinned suite release, implement the same runner protocol, and run it in its own CI without copying private test helpers or modifying this repository.

## User Stories

1. As an ORM maintainer, I want to define expected behavior once, so that adapters cannot silently acquire conflicting semantics.
2. As a Python adapter maintainer, I want shared scenarios to exercise the public Python API, so that conversion and model materialization are covered along with execution.
3. As a TypeScript adapter maintainer, I want the same scenarios to exercise the public TypeScript API, so that parity failures are visible without manually matching test files.
4. As a Rust client maintainer, I want the shared scenarios to exercise generated models and the public client, so that direct engine tests cannot conceal client bugs.
5. As an adapter author in another repository, I want a documented runner protocol and versioned scenario bundle, so that I can verify compatibility without depending on private repository structure.
6. As a database adapter author, I want portable behavioral scenarios to run against my database, so that driver and dialect differences are checked against observable ORM behavior.
7. As a database adapter author, I want to declare supported capabilities, so that unsupported optional behavior is distinguished from a regression.
8. As a feature extension author, I want to contribute scenarios for my feature, so that every adapter claiming support can verify the same contract.
9. As a maintainer, I want enabled and disabled feature behavior tested, so that optional builds expose their advertised surface accurately.
10. As a CI reader, I want results identified by scenario, adapter, runtime, database, and feature profile, so that I can locate the failing combination.
11. As a CI reader, I want failures, unsupported cases, and unavailable infrastructure reported separately, so that an incomplete run cannot appear fully conformant.
12. As an adapter maintainer, I want language-specific tests to remain local, so that shared scenarios do not erase idiomatic behavior or compile-time guarantees.
13. As a suite consumer, I want exact value comparisons for large integers, decimals, dates, and nulls, so that normalization cannot hide data loss.
14. As a maintainer, I want deterministic fixtures and isolated scenario state, so that failures do not depend on test order or another runner's writes.
15. As a release maintainer, I want stable scenario identifiers and explicit suite versions, so that compatibility claims are reproducible.
16. As a future contributor, I want a small example runner, so that adding an adapter does not require reconstructing the existing test harness.

## Implementation Decisions

- The shared suite owns schemas, scenario inputs, operation descriptions, expected outcomes, and the result protocol. Adapters own execution and translation into their public APIs.
- Prefer a small declarative, language-neutral scenario format. Support typed values, named intermediate results, explicit assertions, and transaction blocks. Avoid expanding it into a general programming language.
- Start with JSON scenarios and a machine-readable schema. Version the scenario format separately from the behavioral suite release.
- Runners must use public APIs for behavior under test. Shared scenarios must not directly invoke planner internals, private query serialization, or raw SQL as a substitute for ORM operations. Fixture provisioning may use documented setup facilities.
- Assertions come from the shared suite. Adapter runners may normalize representation, but must not rewrite expected behavior or maintain separate copies of expectations.
- Preserve distinctions between null, absent fields, and unloaded relations. Encode values that cannot safely round-trip as ordinary JSON numbers with explicit type tags. Compare error categories rather than identical exception names or messages.
- Model feature and database support as explicit capability declarations. A missing required baseline capability fails compatibility validation. An absent optional capability produces a visible unsupported result; a claimed capability must pass its scenarios.
- Include negative scenarios that verify unsupported operations produce the documented outcome. Unsupported capabilities do not authorize silently skipping a claimed operation.
- Emit structured results with suite and protocol versions, scenario identifier, adapter/runtime identity, database identity, feature profile, outcome, and diagnostic details. Keep diagnostic logs separate from machine-readable results.
- Keep core tests, adapter-specific tests, database-specific tests, and extension unit tests beside their owning packages. The conformance suite adds shared integration coverage; migrate duplicate tests only after equivalent coverage is demonstrated.
- Make the suite distributable without either existing language package, native build, example application, or repository-specific environment. Each consumer supplies its adapter, database configuration, and fixture provisioning.
- Do not claim that the suite itself makes database adapters externally pluggable. Driver execution, SQL rendering, migrations, and introspection need supported extension boundaries; that architecture is separate work.

## Testing Decisions

- The primary seam is each adapter's public ORM API running against a real supported database. This exercises query construction, language/native conversion, execution, and returned objects together.
- A good scenario specifies inputs and observable outcomes and fails when public behavior changes. It does not assert private representations or require identical generated SQL across databases.
- Start with CRUD, filtering, deterministic ordering, pagination, null handling, scalar round trips, relations, transaction commit and rollback, and constraint failures.
- Add capability-gated coverage for advanced database operations and feature extensions after the baseline protocol works. Cover both enabled behavior and the documented outcome when a feature is unavailable.
- Test lifecycle and compile-time behavior locally: language typing, Rust compile failures, native exception classes, cancellation, and language-specific resource management are not adequately represented by portable JSON steps.
- Reuse behavioral examples from the current Python and TypeScript query, write, relation, connection, and SQLite tests. Engine SQLite and transaction tests provide database prior art; extension-owned public tests provide feature prior art. The existing shared-behavior benchmark is not a general conformance harness.
- Validate malformed scenarios, unknown operations, protocol mismatches, invalid capability declarations, and unsupported result normalization at the harness boundary.
- Verify the suite detects deliberate wrong results and adapter behavior failures. Identical passing summaries alone do not prove that assertions run correctly.
- Give each scenario isolated data and reliable cleanup. Use explicit ordering where order matters, deterministic supplied identifiers, and bounded comparisons for inherently variable values.
- In required CI jobs, an unavailable database fails the job or marks it incomplete. It must not produce a successful conformance claim through automatic skipping.
- Produce a matrix of adapter/runtime × database × feature profile. Begin with supported baseline combinations; use focused extension profiles instead of requiring the full combinatorial matrix.
- Validate external usability in a temporary independent consumer project that receives the suite bundle and public dependencies only. Inspect any dependency on checkout-relative helpers and remove it.

## Out of Scope

- Bulk rearrangement of all existing test directories.
- Implementing a new language client or database dialect as part of the harness.
- Completing the Rust public client described in the related Rust API plan.
- Replacing database-specific migration, introspection, performance, or concurrency stress suites with portable scenarios.
- Requiring identical language syntax, exception classes, or database SQL.
- Benchmarking adapters or promising that baseline conformance proves support for every optional feature.
- Designing a dynamic plugin loader or publishing production packages in this planning task.

## Further Notes

### Delivery order

1. Inventory the baseline behavior already covered by Python and TypeScript and identify the operations needed for a small first scenario collection.
2. Define and validate the scenario schema, typed value rules, capability declarations, result protocol, and suite versioning rules. Include a complete CRUD scenario and expected failure example.
3. Implement Python and TypeScript runners against the public APIs. Run the baseline collection against SQLite and PostgreSQL, including TypeScript runtime coverage for Node and Bun where supported.
4. Integrate structured reporting into CI and document how to run one scenario or one supported combination. Retain existing tests until migrated cases demonstrate equivalent coverage.
5. Add the public Rust runner alongside delivery of the Rust client. Until then, report Rust public conformance as pending rather than implying three-adapter parity.
6. Add focused feature scenario packs and selected database-specific capability scenarios.
7. Package a pinned suite bundle and prove consumption from an independent project. Use that experience to document the requirements for external language and database adapter authors.

### Acceptance criteria

- A behavioral scenario is authored once and executed by the implemented public language runners without duplicating its expected outcomes.
- A deliberate adapter regression produces a failure with the scenario identifier and failing assertion.
- Exact scalar values and null/absent/unloaded distinctions survive normalization.
- Supported capabilities are tested, absent optional capabilities are reported, and missing baseline support fails compatibility validation.
- Required CI combinations cannot pass solely because database access failed and tests were skipped.
- An independent consumer can validate an adapter using the suite bundle and documented interfaces without importing private repository helpers.
- Full three-language parity is claimed only after the Rust public client runner passes the applicable scenarios.

### Related plans

- [Rust public API](09-rust-api.md): public Rust client availability and compile-time behavior.
- [Build-time extensions](01-build-time-extensions.md): feature selection and extension contracts.
- [Optional packaging](02-optional-packaging.md): independently selected native distributions.

The scope assumes the three target languages are Python, TypeScript, and Rust, based on the current suites and Rust API plan.
The external database proof still depends on making the database integration boundaries consumable outside this repository.
