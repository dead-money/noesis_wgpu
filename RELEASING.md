# Releasing

## CI

- **`fmt`**, **`doc`** and **`no shim`** run on hosted runners for every push
  and PR, including forks. Without the `shim` feature the crate needs no Noesis
  SDK, so `no shim` builds, lints, and runs the GPU tests there on lavapipe, a
  software Vulkan driver. `doc` sets `DOCS_RS=1`, which makes
  `noesis_runtime`'s build script skip the native build.
- **`build • clippy • test`** runs with all features on the self-hosted SDK
  runner (label `noesis-sdk`) for pushes, tags, and same-repo PRs. Fork PRs are
  skipped.

## `noesis_runtime` versions

`Cargo.toml` takes `noesis_runtime` from crates.io, and CI checks out this
repo alone. When a change needs an unreleased `noesis_runtime`, develop
against the sibling checkout with a local patch, and publish `noesis_runtime`
first:

```sh
cargo test --config 'patch.crates-io.noesis_runtime.path="../noesis_runtime"'
```

## Release lines

wgpu types are part of the API, so each wgpu major gets its own minor:

- `main` tracks the newest wgpu.
- `release/0.N` keeps an older line for `noesis_bevy`, which follows Bevy's
  wgpu. Cherry-pick fixes onto it only when a `noesis_bevy` release needs them.

Update the version table in `README.md` when a line starts or ends.

## Cutting a release

Requires [cargo-release](https://github.com/crate-ci/cargo-release). With the
branch clean and CI green (use `minor` to start a new line, or no level to
release the version already in `Cargo.toml`):

```sh
cargo release patch --dry-run
cargo release patch --execute
```

It bumps the version, stamps `CHANGELOG.md`, commits, tags `vX.Y.Z`, and
pushes. It doesn't update the link references at the bottom of
`CHANGELOG.md`: move the `[Unreleased]` link to the new tag and add the new
version's compare link by hand. The tag triggers `release.yml` on the SDK
runner, which tests and publishes through crates.io Trusted Publishing.
Afterward, check the crate page and the docs.rs build.

Keep `## [Unreleased]` in `CHANGELOG.md` current as PRs land.
