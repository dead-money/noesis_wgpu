# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). While the crate is
pre-1.0, any `0.x` release may contain breaking changes.

## [Unreleased]

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

[Unreleased]: https://github.com/dead-money/noesis_wgpu/compare/v0.1.0...release/0.1
[0.1.0]: https://github.com/dead-money/noesis_wgpu/releases/tag/v0.1.0
