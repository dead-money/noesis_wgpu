# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). While the crate is
pre-1.0, any `0.x` release may contain breaking changes.

## [Unreleased]

### Changed

- **Breaking:** moved to wgpu 30. The 0.1 line stays on wgpu 29 for Bevy 0.19.
- Protocol violations no longer panic. An unknown handle, a draw outside a
  phase or without a target, a missing batch texture, geometry past the mapped
  buffers, a mismatched tile, or an unbalanced map logs a warning and skips
  the call or the draw. A batch whose shader the device doesn't implement is
  skipped with one warning per shader, and an MSAA render target is created
  single-sampled.

### Added

- `WgpuRenderDevice::stats` returns a `DeviceStats`: batches drawn, batches
  skipped for a protocol violation or an unsupported shader, and pipelines
  compiled. Subtracting two snapshots gives one frame's counts.

### Fixed

- A render phase is no longer limited to 1024 draws. The per-draw uniform rings
  double when they fill instead of panicking.

## [0.1.0]

First release, on wgpu 29. The render device from `noesis_bevy` 0.15, as a
crate of its own:

- `WgpuRenderDevice` implements Noesis's render-device protocol on a
  `wgpu::Device` and `wgpu::Queue` you provide, with logging through `log`.
- The protocol methods are inherent, and `draw_batch_with` takes the batch's
  textures as `BatchTextures` handles, so a host that creates Noesis's textures
  itself can drive the device without `noesis_runtime`'s shim.
- The default `shim` feature implements `noesis_runtime`'s `RenderDevice`
  trait. Without it the crate builds with no Noesis SDK.

[Unreleased]: https://github.com/dead-money/noesis_wgpu/commits/main
