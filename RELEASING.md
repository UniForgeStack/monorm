# Releasing monorm

A `v*` tag runs `.github/workflows/release.yml`.
The workflow publishes 15 PyPI projects, 28 npm packages and the crate `monorm-contracts`.
A manual run (`workflow_dispatch`) publishes to TestPyPI and runs `npm publish --dry-run` and `cargo publish --dry-run`.
The registries match the workflow file name `release.yml`, so every publish step stays in that file.

## Packages

- PyPI, 15 projects: `monorm`, `monorm-auth`, `monorm-native-postgres`, `monorm-native-sqlite`, `monorm-native-combined`, `monorm-native-tooling`, and `monorm-<extension>` for composition, file-storage, generic, inherit, locking, proxy, query-defaults, soft-delete and timestamps.
- npm, 28 packages: `@ufstack/monorm`, the unscoped `monorm` (the same package: it depends on `@ufstack/monorm` at the same version and re-exports it), `@ufstack/monorm-auth`, the 9 `@ufstack/monorm-<extension>` packages, the 4 `@ufstack/monorm-native-<profile>` packages, and 12 `@ufstack/monorm-native-<profile>-<platform>` packages.
- crates.io, 1 crate: `monorm-contracts`.
- `scripts/release/publish-lists.py` holds these lists.
  The `collect` job fails if a file is missing or extra.

## Owner setup

Common values: owner `UniForgeStack`, repository `monorm`, workflow `release.yml`, environment `release`.
"Admin" needs admin rights on `UniForgeStack/monorm`.
"Owner" needs the registry account (npm and PyPI organization `ufstack`, and the crates.io account).

| # | Step | Who | Values |
|---|---|---|---|
| 1 | Create the environment `release` | Admin | Settings → Environments → New environment → `release` |
| 2 | Add a required reviewer | Admin | the owner; leave "Prevent self-review" off, so the owner can approve their own tag |
| 3 | Limit deployment branches and tags | Admin | "Selected branches and tags": tag `v*`, branch `main` and branch `release/*` (for the proof branch) |
| 4 | Optional: tag ruleset for `v*` | Admin | only the owner creates `v*` tags |
| 5 | Create the PyPI pending publisher of `monorm` | Owner | pypi.org → organization `ufstack` → Publishing (or Account → Publishing while the organization is not approved); project name `monorm`; the common values |
| 5a | First PyPI upload of the other 14 projects | Owner, Admin | A PyPI API token with the scope "Entire account". The Admin stores it as the secret `PYPI_API_TOKEN` of the environment `release`. |
| 6 | Create the TestPyPI pending publisher of `monorm`, and the TestPyPI token | Owner, Admin | test.pypi.org → Account → Publishing; the same values. The token goes in the secret `TEST_PYPI_API_TOKEN`. |
| 7 | First npm publish | Owner, Admin | A granular token with read and write access to the scope `@ufstack` and to the new package `monorm` (a scope token does not cover an unscoped name), and a 7-day expiry. The Admin stores it as the secret `NPM_TOKEN` of the environment `release`. |
| 8 | Create 28 npm trusted publishers | Owner | After v0.1.0: `npm trust github <package> --file release.yml --repo UniForgeStack/monorm --env release --allow-publish` for each package. This needs npm 11.15.0 or newer and 2FA. |
| 9 | Remove the npm token | Owner, Admin | Delete the secret `NPM_TOKEN`, revoke the token, and set each package to "Require two-factor authentication and disallow tokens". |
| 9a | Remove the PyPI tokens | Owner, Admin | Add a trusted publisher with the common values on each of the 14 projects (project → Settings → Publishing). Then delete the secrets `PYPI_API_TOKEN` and `TEST_PYPI_API_TOKEN` and the tokens. |
| 10 | First crates.io publish | Owner | Before the tag push, run `cargo publish -p monorm-contracts` from the release commit (the commit the tag will point to). The workflow then skips the version. |
| 11 | crates.io trusted publisher | Owner | crates.io → `monorm-contracts` → Settings → Trusted Publishing; the common values |
| 12 | Optional: "Trusted Publishing only" | Owner | After step 11, in the crates.io settings of `monorm-contracts` |

Keep these points in mind:
- PyPI refuses a pending publisher whose values match a pending publisher of a different project name.
  All 15 projects share the common values, so only `monorm` has one.
  The other 14 projects get their first upload through the token of step 5a.
  Without the secret, the upload uses trusted publishing.
- A PyPI pending publisher does not reserve the name.
  It becomes invalid if someone else registers the name first.
  Do steps 5 and 6 right before the first upload.
- npm does not check a trusted publisher when you save it.
  A wrong value shows only at publish time, so copy the strings from the table.
- A new npm trusted publisher must "complete its first successful publish within 2 days".
  If it does not, it expires and npm does not accept it.
  Delete it and create it again.
- npm allows 10 trusted publishers for each package.
- The crates.io token step is a fallback.
  Before the trusted publisher exists, the `crates-io-auth-action` step fails, and the secret `CARGO_REGISTRY_TOKEN` of the environment `release` takes over.
- Trusted publishing on npm needs npm 11.5.1 or newer and Node 22.14.0 or newer.
  The `publish-npm` job installs `npm@>=11.5.1`, and the install fails if no such version exists.

## Proof sequence

Do this before the first real tag.
The proof publishes only to TestPyPI.

1. Do owner-setup steps 1 to 3 and step 6.
2. Merge the branch that adds `release.yml` into `main`.
   GitHub runs a manual workflow only when the workflow file is on the default branch.
3. Make the branch `release/0.1.0rc1` from `main`.
   It matches the deployment rule of owner-setup step 3.
   That rule lets every `release/*` branch start a deployment, so the reviewer of owner-setup step 2 must read the branch name before approval.
4. On the branch, run `python3 scripts/release/versions.py set 0.1.0rc1` and commit.
   `versions.py set` writes the PEP 440 form (`0.1.0rc1`) in the Python files and the semver form (`0.1.0-rc.1`) in the npm, Cargo and manifest files.
   It also writes the `Cargo.lock` entries of the three crates that change version, so no `cargo update` is needed.
5. Push the branch.
   Run the workflow `Release` by dispatch from the branch.
   The workflow takes the version from `pyproject.toml` and accepts `## [0.1.0] - TBD` in the changelog.
6. Approve the deployment of the environment `release`.
7. Expected result: 15 projects at `0.1.0rc1` on TestPyPI, 28 `npm publish --dry-run` logs, and one `cargo publish --dry-run` log.
8. In a fresh virtual environment on Linux and on macOS, run:
   `pip install --index-url https://test.pypi.org/simple/ --extra-index-url https://pypi.org/simple/ "monorm[postgres]==0.1.0rc1"`.
9. A version on TestPyPI is permanent.
   After a fix, use `0.1.0rc2`.

If TestPyPI rejects the compressed platform tag of an extension wheel, change it back to `py3-none-any` at all of these sites, or CI fails:
- `crates/extension-package/src/package.py`: the value of `tags["platforms"]`, and `Root-Is-Purelib`, which becomes `true`.
- `scripts/check-assembled-packages.py`: the `TAG` constant.
- `crates/extension-package/tests/package.rs`: the wheel file names and the `Tag:` check.
- `docs/contracts/optional-packaging.md`: the wheel name of `assemble` and the paragraph about the compressed tag.

This is the one result that the CI of the branch cannot prove.

## Real release

1. Run `python3 scripts/release/versions.py set 0.1.0`.
   Replace `TBD` in the `## [0.1.0]` heading of `CHANGELOG.md` with the date.
   Commit these files on `main`.
2. Do owner-setup steps 5, 5a, 7 and 10.
3. Push the tag `v0.1.0`.
   The workflow reads the version with `versions.py tag`, which rejects a tag in the PEP 440 form, such as `v0.1.0rc1`.
   Then it runs `versions.py check 0.1.0`, which rejects `TBD`.
4. Approve the deployment of the environment `release`.
   The three publish jobs wait for the same approval.
5. The job `release-notes` creates the GitHub Release from the `CHANGELOG.md` section.
   It sets "prerelease" while the major version is 0.
6. After the first release, do owner-setup steps 8, 9, 9a, 11 and 12.

## Rerun rules

- A published version is permanent on PyPI, npm and crates.io.
  The registries refuse a second upload of the same version, even after a deletion.
- Each publish step skips a version that is already on the registry: PyPI with `skip-existing`, npm with `npm view`, crates.io with the crates.io API.
- If a publish job fails before it finishes, use "Re-run failed jobs".
  The rerun publishes only what is missing.
- If a published file is wrong, do not rerun.
  Make a new version (`0.1.1`) and a new tag.
- PyPI gets the thin `monorm` project last.
  A failure before it leaves no `monorm` that points to a missing native wheel.
- On npm, a failure can leave platform packages without `@ufstack/monorm`.
  This is safe, because nothing points to them.
- A TestPyPI run that fails halfway also needs only a rerun of the failed jobs.
  A run after a code change needs a new rc version.

## Checks that publish nothing

```bash
python3 -m unittest scripts/tests/test_release_versions.py scripts/tests/test_check_glibc.py
python3 scripts/release/versions.py check --allow-tbd
cargo package -p monorm-contracts
cargo publish --dry-run -p monorm-contracts
```

`scripts/check-glibc.py` runs in the Linux legs of `package-profiles.yml` and `extension-packages.yml`.
It fails when a Linux build needs a glibc symbol newer than 2.28, and when an argument holds no library, so a wrong glob does not pass.
