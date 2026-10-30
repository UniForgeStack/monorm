# Releasing monorm

A `v*` tag runs `.github/workflows/release.yml`.
The workflow publishes one PyPI project and one npm package.
A manual run (`workflow_dispatch`) publishes to TestPyPI and runs `npm publish --dry-run`.
crates.io comes later, when the Rust API is ready. The crate `monorm` (the directory `crates/contracts`) is ready for it, and `cargo package -p monorm` checks it.
The registries match the workflow file name `release.yml`, so every publish step stays in that file.

## Packages

- PyPI, 1 project: `monorm`, 16 wheels (Linux x86-64, Linux arm64, macOS arm64 and Windows x86-64, for CPython 3.11 to 3.14). It has no sdist, because an sdist would lack the extension libraries and the auth app.
  The wheel holds the engine (`monorm._native`), the 9 built-in extension libraries of its platform and the auth app (`monorm_auth`).
- npm, 1 package: `monorm`, one tarball with the addon and the extension libraries of the 4 platforms (`prebuilt/<platform>/` and `extensions/<name>/<platform>/`), the CLI and the auth app.
- `scripts/release/publish-lists.py` holds these lists.
  The `collect` job fails if a file is missing or extra, and records the size of each file in the job summary.
- The jobs: `version`, `builds` (`package-builds.yml`), `extensions` (`extension-packages.yml`), `collect`, then `publish-pypi` and `publish-npm`, then `release-notes`.

## Owner setup

Common values: owner `UniForgeStack`, repository `monorm`, workflow `release.yml`, environment `release`.
"Admin" needs admin rights on `UniForgeStack/monorm`.
"Owner" needs the registry account (npm and PyPI organization `ufstack`).

| # | Step | Who | Values |
|---|---|---|---|
| 1 | Create the environment `release` | Admin | Settings → Environments → New environment → `release` |
| 2 | Add a required reviewer | Admin | the owner; leave "Prevent self-review" off, so the owner can approve their own tag |
| 3 | Limit deployment branches and tags | Admin | "Selected branches and tags": tag `v*`, branch `main` and branch `release/*` (for the proof branch) |
| 4 | Optional: tag ruleset for `v*` | Admin | only the owner creates `v*` tags |
| 5 | Create the PyPI pending publisher of `monorm` | Owner | pypi.org → organization `ufstack` → Publishing (or Account → Publishing while the organization is not approved); project name `monorm`; the common values. PyPI has no token and the workflow has no token path. |
| 6 | Create the TestPyPI pending publisher of `monorm` | Owner | test.pypi.org → Account → Publishing; the same values |
| 7 | First npm publish | Owner, Admin | A granular token with read and write access to the new package `monorm` (a scope token does not cover an unscoped name), and a 7-day expiry. The Admin stores it as the secret `NPM_TOKEN` of the environment `release`. |
| 8 | Create the npm trusted publisher | Owner | After v0.1.0: `npm trust github monorm --file release.yml --repo UniForgeStack/monorm --env release --allow-publish`. This needs npm 11.15.0 or newer and 2FA. |
| 9 | Remove the npm token | Owner, Admin | Delete the secret `NPM_TOKEN`, revoke the token, and set the package to "Require two-factor authentication and disallow tokens". |

Keep these points in mind:
- PyPI has one trusted publisher and no token, so the first upload of `monorm` also uses trusted publishing (a pending publisher).
- A PyPI pending publisher does not reserve the name.
  It becomes invalid if someone else registers the name first.
  Do steps 5 and 6 right before the first upload.
- npm does not check a trusted publisher when you save it.
  A wrong value shows only at publish time, so copy the strings from the table.
- A new npm trusted publisher must "complete its first successful publish within 2 days".
  If it does not, it expires and npm does not accept it.
  Delete it and create it again.
- The npm token step is a fallback.
  Without the secret `NPM_TOKEN`, npm uses the trusted publisher.
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
   It also writes the `Cargo.lock` entries of the three crates that change version (`monorm`, `monorm-python`, `monorm-node`), so no `cargo update` is needed.
5. Push the branch.
   Run the workflow `Release` by dispatch from the branch.
   The workflow takes the version from `pyproject.toml` and accepts `## [0.1.0] - TBD` in the changelog.
6. Approve the deployment of the environment `release`.
7. Expected result: `monorm` at `0.1.0rc1` on TestPyPI with 16 wheels and one `npm publish --dry-run` log.
8. In a fresh virtual environment on Linux and on macOS, run:
   `pip install --index-url https://test.pypi.org/simple/ --extra-index-url https://pypi.org/simple/ monorm==0.1.0rc1`.
   Then run `python -m monorm --help` and a `generate` on a schema that uses one built-in extension.
9. A version on TestPyPI is permanent.
   After a fix, use `0.1.0rc2`.

## Real release

1. Run `python3 scripts/release/versions.py set 0.1.0`.
   Replace `TBD` in the `## [0.1.0]` heading of `CHANGELOG.md` with the date.
   Commit these files on `main`.
2. Do owner-setup steps 5 and 7.
3. Push the tag `v0.1.0`.
   The workflow reads the version with `versions.py tag`, which rejects a tag in the PEP 440 form, such as `v0.1.0rc1`.
   Then it runs `versions.py check 0.1.0`, which rejects `TBD`.
4. Approve the deployment of the environment `release`.
   The two publish jobs wait for the same approval.
5. The job `release-notes` creates the GitHub Release from the `CHANGELOG.md` section.
   It sets "prerelease" while the major version is 0.
6. After the first release, do owner-setup steps 8 and 9.

## Rerun rules

- A published version is permanent on PyPI and npm.
  The registries refuse a second upload of the same version, even after a deletion.
- Each publish step skips a version that is already on the registry: PyPI with `skip-existing`, npm with `npm view`.
- If a publish job fails before it finishes, use "Re-run failed jobs".
  The rerun publishes only what is missing.
- If a published file is wrong, do not rerun.
  Make a new version (`0.1.1`) and a new tag.
- PyPI takes the 16 wheels in one upload with `skip-existing`, so a rerun adds only the missing wheels.
  A wheel for one platform can exist before the others; pip then picks the wheels of the platforms that are up.
- A TestPyPI run that fails halfway also needs only a rerun of the failed jobs.
  A run after a code change needs a new rc version.

## Checks that publish nothing

```bash
python3 -m unittest scripts/tests/test_release_versions.py scripts/tests/test_package_metadata.py scripts/tests/test_check_glibc.py
python3 scripts/release/versions.py check --allow-tbd
cargo package -p monorm
```

`scripts/check-glibc.py` runs in the Linux legs of `package-builds.yml`.
It fails when a Linux build needs a glibc symbol newer than 2.28, and when an argument holds no library, so a wrong glob does not pass.
