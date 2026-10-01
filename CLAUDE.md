# noesis_wgpu

A wgpu render device for the closed-source Noesis GUI SDK. It draws the batches
Noesis hands a `RenderDevice`, on a wgpu device and queue the caller owns.
`noesis_bevy` and Hommlet's `aor_render` both build on it.

## Invariants

- **No `unsafe`.** This crate is `unsafe_code = forbid`. FFI lives in the
  sibling `../noesis_runtime`, which is ours and freely editable.
- **The core builds without the SDK.** Only the `shim` feature (the
  `RenderDevice` impl and `BatchTextures::from_batch`) touches
  `noesis_runtime`'s C++ shim. Keep everything else building and tested with
  `--no-default-features`, and never call `Batch::*_handle()` outside
  shim-gated code: it reads garbage from a texture the shim didn't create.
- **SDK never in the repo.** It lives at `$NOESIS_SDK_DIR` (per-developer
  licensed); never commit any SDK content.
- **Release lines follow wgpu majors.** `main` is on the newest wgpu;
  `release/0.N` branches carry older ones for `noesis_bevy`. See
  [`RELEASING.md`](./RELEASING.md).

## Attribution

Do not add `Co-Authored-By: Claude` trailers to commits or "Generated with
Claude Code" footers to PR bodies. Author lines and PR bodies stay clean.
`scripts/git-hooks/commit-msg` strips these defensively; activate per clone
with `git config core.hooksPath scripts/git-hooks`. Do not work around the
hook.

## Pointers

- GPU tests: `tests/wgpu_suite/`, one test per file. Each requests its own wgpu
  device, draws through the device, and reads pixels back. Plain `cargo test`
  runs them.
- Shaders: `src/shaders/noesis.wgsl`, one variant per Noesis shader through the
  defines in `src/shader_defines.rs`. The `shader_uses_*` lists in
  `src/device.rs` must agree with those defines.
