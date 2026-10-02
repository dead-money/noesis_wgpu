# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). While the crate is
pre-1.0, any `0.x` release may contain breaking changes.

## [Unreleased]

### Added

- The `SDF_LINEAR`, `SDF_RADIAL` and `SDF_PATTERN*` shaders, so text draws
  with gradient and image brushes.
- Custom pixel shaders. `WgpuRenderDevice::create_pixel_shader` compiles WGSL
  for a `BrushShader` or a `ShaderEffect` and returns a `PixelShaderHandle`;
  `draw_custom_batch` draws a batch with it, taking extra textures and
  constants in a `BatchShader`. With the `shim` feature, `draw_batch` reads
  `batch.pixel_shader` as a handle. `drop_pixel_shader` releases one.
- `WgpuRenderDevice::set_pattern_lod` sets a mip bias and a highest mip level
  for images, as a `PatternLod`. The shaders apply it, so it needs no wgpu
  feature.
- `WgpuRenderDevice::import_texture` wraps a `wgpu::Texture` the host owns as
  a texture Noesis can sample, without copying it.
- MSAA render targets. A target created with more than one sample is 4x
  multisampled, and `resolve_render_target` resolves it.

### Changed

- `draw_batch_with` skips a batch whose `pixel_shader` is set, counting it as
  unsupported. Draw those with `draw_custom_batch`.

## [0.2.0] - 2026-10-02

### Changed

- **Breaking:** moved to wgpu 30. The 0.1 line stays on wgpu 29 for Bevy 0.19.
- Protocol violations no longer panic. An unknown handle, a draw outside a
  phase or without a target, a missing batch texture, geometry past the mapped
  buffers, a mismatched tile, an unbalanced map, or a texture update outside
  the texture or short of data logs a warning and skips the call or the draw.
  A tile larger than its render target is clipped to it. A batch whose shader
  the device doesn't implement is skipped with one warning per shader, as is
  `SDF_LCD_SOLID` on a wgpu device without `DUAL_SOURCE_BLENDING`. An MSAA
  render target is created single-sampled.

### Added

- `WgpuRenderDevice::stats` returns a `DeviceStats`: batches drawn, batches
  skipped for a protocol violation or an unsupported shader, and pipelines
  compiled. Subtracting two snapshots gives one frame's counts.

### Fixed

- A render phase is no longer limited to 1024 draws. The per-draw uniform rings
  double when they fill instead of panicking.

## [0.1.0] - 2026-10-02

First release, on wgpu 29. The render device from `noesis_bevy` 0.15, as a
crate of its own:

- `WgpuRenderDevice` implements Noesis's render-device protocol on a
  `wgpu::Device` and `wgpu::Queue` you provide, with logging through `log`.
- The protocol methods are inherent, and `draw_batch_with` takes the batch's
  textures as `BatchTextures` handles, so a host that creates Noesis's textures
  itself can drive the device without `noesis_runtime`'s shim.
- The default `shim` feature implements `noesis_runtime`'s `RenderDevice`
  trait. Without it the crate builds with no Noesis SDK.
- Fixed: mapping more geometry than the stream's staging buffer held, with a
  length that isn't a multiple of 4, no longer panics at unmap.

[Unreleased]: https://github.com/dead-money/noesis_wgpu/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/dead-money/noesis_wgpu/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/dead-money/noesis_wgpu/releases/tag/v0.1.0
