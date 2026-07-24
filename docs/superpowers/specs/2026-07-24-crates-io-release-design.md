# tmux-rescue Crates.io Release Design

## Goal

Publish `tmux-rescue` as a CC0-1.0 Rust CLI package, establish a reproducible
first release at `v0.1.0`, and publish later manifest-version tags to crates.io
through GitHub Actions trusted publishing.

## Scope

- Publish the Cargo package to crates.io.
- Add CI for pull requests and `main`.
- Add tag-triggered crates.io publication for later releases.
- Run the complete Rust test suite, including its isolated real-tmux tests.
- Add the package metadata, `README.md`, and `LICENSE` required for a useful
  public crate.

## Non-goals

- No GitHub Release, release archive, checksum, binary-asset publication, or
  release attestation.
- No automated version bump or tag creation.
- No Markdown, YAML, or license linting or text-based tests.
- No changes to the existing GitHub Pages deployment behavior.

## Package Surface

`Cargo.toml` declares the public package identity and discovery metadata:

- package name and first version: `tmux-rescue` `0.1.0`;
- Rust floor: `1.94`;
- license: `CC0-1.0` with the complete CC0-1.0 legal text in `LICENSE`;
- repository: `https://github.com/huweiATgithub/tmux-rescue`;
- a concise description and CLI-relevant keywords/categories; and
- `README.md` as the rendered crates.io introduction.

The README explains the manual snapshot and plan-first restore model, links to
the published design documentation, states the Linux/tmux requirement, and
shows `cargo install tmux-rescue` plus the two primary command forms. It does
not promise automatic capture or recovery beyond the v1 design.

## CI Contract

`.github/workflows/ci.yml` runs for pull requests and pushes to `main`. It has
read-only repository permissions and uses Ubuntu 24.04, Rust `1.94`, and tmux
`3.4`.

The job explicitly installs tmux, verifies its version, then runs these Rust
checks:

```text
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked
cargo doc --all-features --locked --no-deps
cargo package --locked
```

The test command is intentionally the complete suite. Its normal, non-ignored
`e2e`, `tmux_source`, and `tmux_target` integration tests start only temporary
tmux servers at unique socket paths, so CI exercises the real tmux adapter and
never depends on a runner's default tmux server.

## Release Contract

`.github/workflows/release.yml` triggers on `v*.*.*` tag pushes and has two
ordered jobs:

1. `validate` reruns the release-relevant CI checks, including tmux installation
   and the complete test suite. It parses the sole package version from Cargo
   metadata and fails unless the tag is exactly `v<package-version>`.
2. `publish` runs only after validation. It first checks whether crates.io
   already exposes exactly that package version. If so, it reports an idempotent
   release rerun. Otherwise it exchanges a GitHub OIDC token through
   `rust-lang/crates-io-auth-action@v1` and executes `cargo publish --locked`.

The publish job uses the GitHub Actions environment `crates-io` and only the
permissions needed to read repository contents and request an OIDC token.
It has no long-lived crates.io token. A tag/version mismatch, failed package
check, unavailable trusted-publishing configuration, or failed registry upload
fails visibly and creates no substitute release artifact.

## First-release Bootstrap

crates.io requires the first version to be published manually before its
trusted-publisher settings can protect later releases. The `v0.1.0` sequence
is therefore:

1. Commit and push the v1 implementation and release configuration to `main`.
2. Run the full local release checks from that exact commit.
3. Publish `tmux-rescue 0.1.0` with `cargo publish --locked` using the
   maintainer's local crates.io authentication.
4. Verify the immutable `0.1.0` package is visible on crates.io.
5. Configure crates.io trusted publishing for repository
   `huweiATgithub/tmux-rescue`, workflow `.github/workflows/release.yml`, and
   environment `crates-io`.
6. Create and push annotated tag `v0.1.0`; the workflow validates the tag and
   recognizes the already-published version as an idempotent bootstrap run.

Subsequent releases bump `Cargo.toml`, commit, and push a matching annotated
tag. The tag workflow publishes the crate automatically after its complete
validation succeeds.

## Verification Boundary

Release confidence comes from Rust compilation, linting, documentation build,
Cargo packaging, and the complete functional test suite against tmux. Workflow,
README, and license files are reviewed as part of the normal diff and exercised
by the actual GitHub Actions run; they do not receive separate text linting or
text-based tests.
