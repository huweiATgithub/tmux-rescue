# tmux-rescue Crates.io Release Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish `tmux-rescue` 0.1.0 to crates.io and make later matching version tags publish through GitHub OIDC trusted publishing.

**Architecture:** The Cargo manifest owns public package metadata and the source-package boundary. A pull-request/main CI workflow verifies the complete Rust product against tmux 3.4. A separate tag workflow reruns that validation, proves tag/manifest coherence, and publishes only through crates.io; the first manual publication seeds the registry for later OIDC publishing.

**Tech Stack:** Rust 1.94, Cargo, tmux 3.4, GitHub Actions, crates.io trusted publishing, CC0-1.0.

## Global Constraints

- Work directly on the user-approved `main` checkout.
- Publish only to crates.io; do not create GitHub Releases or binary assets.
- Use `CC0-1.0` and include the canonical CC0-1.0 legal text in `LICENSE`.
- CI uses Ubuntu 24.04, Rust 1.94, and explicitly installed tmux 3.4.
- CI and release validation run `cargo test --all-targets --all-features --locked`, including real isolated-tmux integration tests.
- Do not add Markdown, YAML, or license linting or text-based tests.
- Do not change the existing GitHub Pages deployment workflow.
- Configuration and documentation files are verified by Cargo/package checks and the real GitHub Actions run, not test-first unit tests; the user explicitly approved this exception.

## File Map

- `Cargo.toml`: crates.io metadata and the exact source-package inclusion list.
- `LICENSE`: CC0-1.0 legal text.
- `README.md`: crates.io-visible purpose, requirements, installation, and command examples.
- `.github/workflows/ci.yml`: read-only complete Rust/tmux validation for pull requests and `main`.
- `.github/workflows/release.yml`: tag validation and OIDC-authenticated crates.io publication.
- `docs/superpowers/specs/2026-07-24-crates-io-release-design.md`: approved release contract.

---

### Task 1: Define the publishable Cargo package

**Files:**
- Modify: `Cargo.toml`
- Create: `LICENSE`
- Create: `README.md`

**Interfaces:**
- Produces: a `tmux-rescue` crates.io package with version `0.1.0`, SPDX license expression `CC0-1.0`, repository URL, readme, discovery metadata, and a bounded source archive.
- Consumed by: `cargo package --locked`, manual `cargo publish --locked`, and the tag-release workflow.

- [x] **Step 1: Add package metadata and source-package boundaries**

Add these fields beneath the existing package identity without changing the
dependency list:

```toml
description = "Snapshot and restore tmux session topology and recoverable foreground commands"
license = "CC0-1.0"
repository = "https://github.com/huweiATgithub/tmux-rescue"
readme = "README.md"
keywords = ["tmux", "terminal", "recovery", "session", "backup"]
categories = ["command-line-utilities", "development-tools"]
include = [
    "Cargo.toml",
    "Cargo.lock",
    "LICENSE",
    "README.md",
    "src/**",
    "tests/**",
]
```

- [x] **Step 2: Add public package documents**

Add the canonical CC0-1.0 legal text to `LICENSE`. Create a concise README
that states the tool snapshots one tmux server, restore is plan-first, v1 is
Linux/tmux 3.4-oriented, and installation is:

```bash
cargo install tmux-rescue
tmux-rescue snapshot
tmux-rescue restore --target /tmp/tmux-rescue.sock
tmux-rescue restore --target /tmp/tmux-rescue.sock --run
```

Link the repository's published design documentation without promising
unsupported automatic capture or exact layout restoration.

- [x] **Step 3: Preview the generated source package before the v1 commit**

Run:

```bash
cargo package --locked --allow-dirty
cargo package --locked --allow-dirty --list
```

Expected: Cargo creates a preview `tmux-rescue-0.1.0.crate`; its list contains
the declared source-package surface plus Cargo's generated
`.cargo_vcs_info.json` and `Cargo.toml.orig`, and it reports no missing
required crates.io metadata. The clean-tree package gate occurs after the v1
commit.

- [x] **Step 4: Retain the package surface for the single v1 commit**

Do not create a partial package commit. The currently uncommitted v1 source,
tests, design-contract updates, and release configuration form one release-ready
implementation commit in Task 4.

### Task 2: Add complete Rust and tmux CI

**Files:**
- Create: `.github/workflows/ci.yml`

**Interfaces:**
- Produces: the `CI` GitHub Actions workflow for pull requests and `main`.
- Consumed by: contributors and release validation expectations.

- [x] **Step 1: Define read-only CI triggers and runner**

Create a workflow named `CI` with these top-level controls:

```yaml
on:
  pull_request:
  push:
    branches:
      - main

permissions:
  contents: read

concurrency:
  group: ci-${{ github.ref }}
  cancel-in-progress: true
```

Use one `ubuntu-24.04` job. Check out with `actions/checkout@v6`, install
toolchain `1.94.0` with `rustfmt,clippy` through
`dtolnay/rust-toolchain@1.94.0`, and cache Cargo with
`Swatinem/rust-cache@v2`.

- [x] **Step 2: Require real tmux and run the entire Rust suite**

Add a tmux setup step and the exact validation commands:

```yaml
- name: Install tmux 3.4
  run: |
    sudo apt-get update
    sudo apt-get install --yes tmux
    tmux_version="$(tmux -V)"
    printf '%s\n' "$tmux_version"
    test "$tmux_version" = "tmux 3.4"

- name: Check formatting
  run: cargo fmt --all -- --check

- name: Run Clippy
  run: cargo clippy --all-targets --all-features --locked -- -D warnings

- name: Run full test suite
  run: cargo test --all-targets --all-features --locked

- name: Build Rust documentation
  run: cargo doc --all-features --locked --no-deps

- name: Package crate
  run: cargo package --locked
```

The full test command must remain a normal test invocation: do not filter,
ignore, mock, or otherwise exclude the `e2e`, `tmux_source`, or `tmux_target`
tests.

- [x] **Step 3: Verify the CI command contract locally**

Run the same Rust commands locally after installing/confirming tmux 3.4:

```bash
tmux -V
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked
cargo doc --all-features --locked --no-deps
cargo package --locked --allow-dirty
```

Expected: every command exits zero and the test command runs the real isolated
tmux integration-test binaries.

- [ ] **Step 4: Retain CI for the single v1 commit**

Do not create a separate CI commit. Task 4 commits the tested implementation
and both release workflows together.

### Task 3: Add tag-triggered crates.io publication

**Files:**
- Create: `.github/workflows/release.yml`

**Interfaces:**
- Consumes: a pushed tag matching `v<package-version>` and the validated Cargo
  package.
- Produces: a crates.io publication or an explicit idempotent result if that
  exact version is already immutable on crates.io.

- [x] **Step 1: Add release validation**

Create a `Release` workflow for `v*.*.*` tag pushes with read-only default
permissions and non-cancelling per-tag concurrency:

```yaml
on:
  push:
    tags:
      - "v*.*.*"

permissions:
  contents: read

concurrency:
  group: release-${{ github.ref }}
  cancel-in-progress: false
```

Its `validate` job repeats Task 2's checkout, Rust, tmux, format, Clippy,
complete test, documentation, and package steps. Add a `package-version` step
whose shell is:

```bash
version="$(cargo pkgid --locked --package tmux-rescue)"
version="${version##*#}"
if ! [[ "$GITHUB_REF_NAME" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  printf 'unsupported release tag: %s\n' "$GITHUB_REF_NAME" >&2
  exit 1
fi
if [ "$GITHUB_REF_NAME" != "v$version" ]; then
  printf 'tag %s does not match package version %s\n' "$GITHUB_REF_NAME" "$version" >&2
  exit 1
fi
printf 'version=%s\n' "$version" >> "$GITHUB_OUTPUT"
```

Export that step's `version` as the job output named `version`.

- [x] **Step 2: Add OIDC publication with safe idempotence**

Add a `publish` job that needs `validate`, declares
`environment: crates-io`, and grants only:

```yaml
permissions:
  contents: read
  id-token: write
```

After checkout and Rust installation, use this registry check:

```bash
url="https://crates.io/api/v1/crates/tmux-rescue/${{ needs.validate.outputs.version }}"
user_agent="tmux-rescue-release/${{ needs.validate.outputs.version }}"
registry_http_status="$(curl --silent --show-error --location --user-agent "$user_agent" --output /dev/null --write-out '%{http_code}' "$url")"
case "$registry_http_status" in
  200) printf 'already_published=true\n' >> "$GITHUB_OUTPUT" ;;
  404) printf 'already_published=false\n' >> "$GITHUB_OUTPUT" ;;
  *) printf 'crates.io version lookup failed with HTTP status %s\n' "$registry_http_status" >&2; exit 1 ;;
esac
```

Give that step id `registry`. Run `rust-lang/crates-io-auth-action@v1` with id
`auth` only when `steps.registry.outputs.already_published != 'true'`. Run:

```yaml
- name: Publish to crates.io
  if: steps.registry.outputs.already_published != 'true'
  run: cargo publish --locked
  env:
    CARGO_REGISTRY_TOKEN: ${{ steps.auth.outputs.token }}
```

Add a final explicit message for the exact already-published bootstrap case.
Do not create an asset, GitHub Release, checksum, or fallback token path.

- [x] **Step 3: Inspect the workflow and release commands without text linting**

Review the staged diff, then run the tag-version extraction locally using a
synthetic matching tag name:

```bash
GITHUB_REF_NAME=v0.1.0 bash -ceu '
version="$(cargo pkgid --locked --package tmux-rescue)"
version="${version##*#}"
[[ "$GITHUB_REF_NAME" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]
[ "$GITHUB_REF_NAME" = "v$version" ]
'
```

Expected: the release boundary accepts `v0.1.0`; a real tag run is the
workflow-level verification. Do not add YAML linting or text tests.

- [ ] **Step 4: Retain release automation for the single v1 commit**

Do not create a separate release-automation commit. Task 4 commits the tested
implementation and both release workflows together.

### Task 4: Commit the release-ready v1 implementation

**Files:**
- Add: `.gitignore`, `Cargo.toml`, `Cargo.lock`, `LICENSE`, `README.md`, `src/**`, and `tests/**`
- Modify: `docs/src/ARCHITECTURE.md`, `docs/src/TOOL-RECOVERIES.md`
- Add: `docs/superpowers/plans/2026-07-23-v1-implementation.md`
- Add: `docs/superpowers/plans/2026-07-24-crates-io-release-implementation.md`
- Create: `.github/workflows/ci.yml`, `.github/workflows/release.yml`

**Interfaces:**
- Consumes: the verified source tree and the release configuration from Tasks 1–3.
- Produces: one clean, reviewable `main` commit that can be packaged and
  published without Cargo's dirty-tree override.

- [ ] **Step 1: Stage only the approved v1 and release surface**

```bash
git add \
  .gitignore Cargo.toml Cargo.lock LICENSE README.md src tests \
  docs/src/ARCHITECTURE.md docs/src/TOOL-RECOVERIES.md \
  docs/superpowers/plans/2026-07-23-v1-implementation.md \
  docs/superpowers/plans/2026-07-24-crates-io-release-implementation.md \
  .github/workflows/ci.yml .github/workflows/release.yml
git diff --cached --check
git diff --cached --stat
```

Expected: only the approved v1 implementation, its authoritative contract
updates, package documents, and CI/release files are staged. The already
committed release design spec is not restaged.

- [ ] **Step 2: Commit the complete release candidate**

```bash
git commit -m "feat: implement tmux-rescue v1"
```

Expected: the commit leaves no tracked or untracked v1/release files behind.

### Task 5: Release v0.1.0 and establish future trusted publishing

**Files:**
- Modify: no source files

**Interfaces:**
- Consumes: clean `main`, a public `tmux-rescue 0.1.0` package name, and the
  maintainer's crates.io account.
- Produces: immutable crates.io version `0.1.0`, annotated tag `v0.1.0`, and
  a passing bootstrap release workflow.

- [ ] **Step 1: Push the committed release candidate and wait for CI**

```bash
git status --short
git push origin main
run_id="$(gh run list --branch main --workflow CI --limit 1 --json databaseId --jq '.[0].databaseId')"
test -n "$run_id"
gh run watch "$run_id" --exit-status
```

Expected: the working tree is clean and the pushed `CI` run passes its complete
real-tmux test suite before any irreversible registry action.

- [ ] **Step 2: Run the complete local release gate from committed `main`**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked
cargo doc --all-features --locked --no-deps
cargo package --locked
```

Expected: the working tree is clean and every command succeeds before any
irreversible registry action.

- [ ] **Step 3: Publish the bootstrap version manually**

```bash
cargo publish --locked
```

Expected: crates.io accepts immutable version `0.1.0`. If local authentication
or package-name ownership is unavailable, stop without creating the tag and
report the external prerequisite.

- [ ] **Step 4: Configure trusted publishing after the first publication**

In the crates.io settings for `tmux-rescue`, configure the GitHub trusted
publisher as repository `huweiATgithub/tmux-rescue`, workflow
`.github/workflows/release.yml`, and environment `crates-io`. This one-time
web-account operation cannot be performed from repository source control.

- [ ] **Step 5: Tag, push, and verify the bootstrap workflow**

```bash
git tag -a v0.1.0 -m "Release v0.1.0"
git push origin v0.1.0
```

Verify the `Release` tag run is successful, then confirm crates.io exposes
exactly `tmux-rescue 0.1.0`.
