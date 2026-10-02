//! The WGSL define set for each Noesis [`Shader`], which selects that shader's
//! variant of `noesis.wgsl` through [`preprocess`].
//!
//! [`preprocess`]: crate::shader_preproc::preprocess

use std::collections::HashSet;

use noesis_runtime::render_device::types::Shader;

use crate::custom_shader::PixelShaderKind;

/// Returns the WGSL define set for `shader`, or `None` when `noesis.wgsl` has
/// no variant for it: every `SDF_LCD_*` except `SDF_LCD_SOLID`, and
/// `CUSTOM_EFFECT`, which only draws through a custom shader (see
/// [`defines_for_custom`]).
#[must_use]
#[allow(clippy::too_many_lines)] // one arm per shader variant, no abstraction buys clarity here
pub fn defines_for_shader(shader: Shader) -> Option<HashSet<&'static str>> {
    let mut d: HashSet<&'static str> = HashSet::new();
    match shader.0 {
        n if n == Shader::RGBA.0 => {
            d.insert("EFFECT_RGBA");
        }
        n if n == Shader::MASK.0 => {
            d.insert("EFFECT_MASK");
        }
        n if n == Shader::CLEAR.0 => {
            d.insert("EFFECT_CLEAR");
        }

        n if n == Shader::PATH_SOLID.0 => {
            d.insert("HAS_COLOR");
            d.insert("PAINT_SOLID");
            d.insert("EFFECT_PATH");
        }

        n if n == Shader::PATH_AA_SOLID.0 => {
            d.insert("HAS_COLOR");
            d.insert("HAS_COVERAGE");
            d.insert("PAINT_SOLID");
            d.insert("EFFECT_PATH_AA");
        }

        // PAINT_PATTERN_PLAIN gates the no-wrap branch; the wrap variants
        // below gate their own blocks instead.
        n if n == Shader::PATH_PATTERN.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("PAINT_PATTERN_PLAIN");
            d.insert("EFFECT_PATH");
        }

        n if n == Shader::PATH_AA_PATTERN.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_COVERAGE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("PAINT_PATTERN_PLAIN");
            d.insert("EFFECT_PATH_AA");
        }

        // HAS_RECT / HAS_TILE must match the SDK's FORMAT_FOR_VERTEX: CLAMP is
        // PosTex0Rect, REPEAT and MIRROR* are PosTex0RectTile, AA adds coverage.
        n if n == Shader::PATH_PATTERN_CLAMP.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_RECT");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("CLAMP_PATTERN");
            d.insert("EFFECT_PATH");
        }
        n if n == Shader::PATH_AA_PATTERN_CLAMP.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_COVERAGE");
            d.insert("HAS_RECT");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("CLAMP_PATTERN");
            d.insert("EFFECT_PATH_AA");
        }
        n if n == Shader::PATH_PATTERN_REPEAT.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_RECT");
            d.insert("HAS_TILE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("REPEAT_PATTERN");
            d.insert("EFFECT_PATH");
        }
        n if n == Shader::PATH_AA_PATTERN_REPEAT.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_COVERAGE");
            d.insert("HAS_RECT");
            d.insert("HAS_TILE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("REPEAT_PATTERN");
            d.insert("EFFECT_PATH_AA");
        }
        n if n == Shader::PATH_PATTERN_MIRROR_U.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_RECT");
            d.insert("HAS_TILE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("MIRRORU_PATTERN");
            d.insert("EFFECT_PATH");
        }
        n if n == Shader::PATH_AA_PATTERN_MIRROR_U.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_COVERAGE");
            d.insert("HAS_RECT");
            d.insert("HAS_TILE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("MIRRORU_PATTERN");
            d.insert("EFFECT_PATH_AA");
        }
        n if n == Shader::PATH_PATTERN_MIRROR_V.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_RECT");
            d.insert("HAS_TILE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("MIRRORV_PATTERN");
            d.insert("EFFECT_PATH");
        }
        n if n == Shader::PATH_AA_PATTERN_MIRROR_V.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_COVERAGE");
            d.insert("HAS_RECT");
            d.insert("HAS_TILE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("MIRRORV_PATTERN");
            d.insert("EFFECT_PATH_AA");
        }
        n if n == Shader::PATH_PATTERN_MIRROR.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_RECT");
            d.insert("HAS_TILE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("MIRROR_PATTERN");
            d.insert("EFFECT_PATH");
        }
        n if n == Shader::PATH_AA_PATTERN_MIRROR.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_COVERAGE");
            d.insert("HAS_RECT");
            d.insert("HAS_TILE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("MIRROR_PATTERN");
            d.insert("EFFECT_PATH_AA");
        }

        n if n == Shader::PATH_LINEAR.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_LINEAR");
            d.insert("EFFECT_PATH");
        }

        n if n == Shader::PATH_AA_LINEAR.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_COVERAGE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_LINEAR");
            d.insert("EFFECT_PATH_AA");
        }

        n if n == Shader::PATH_RADIAL.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_RADIAL");
            d.insert("EFFECT_PATH");
        }

        n if n == Shader::PATH_AA_RADIAL.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_COVERAGE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_RADIAL");
            d.insert("EFFECT_PATH_AA");
        }

        // The glyph atlas sits at group(3), leaving group(2) for the paint.
        n if n == Shader::SDF_SOLID.0 => {
            d.insert("HAS_COLOR");
            d.insert("HAS_UV1");
            d.insert("HAS_ST1");
            d.insert("HAS_GLYPHS_TEXTURE");
            d.insert("PAINT_SOLID");
            d.insert("EFFECT_SDF");
        }

        // The SDF paints take the vertex formats of their PATH twins plus uv1:
        // PosTex0Tex1, PosTex0Tex1Rect for CLAMP, PosTex0Tex1RectTile for
        // REPEAT and MIRROR*.
        n if n == Shader::SDF_LINEAR.0 => {
            d.extend(SDF_PAINT);
            d.insert("PAINT_LINEAR");
        }
        n if n == Shader::SDF_RADIAL.0 => {
            d.extend(SDF_PAINT);
            d.insert("PAINT_RADIAL");
        }
        n if n == Shader::SDF_PATTERN.0 => {
            d.extend(SDF_PAINT);
            d.extend(["PAINT_PATTERN", "PAINT_PATTERN_PLAIN"]);
        }
        n if n == Shader::SDF_PATTERN_CLAMP.0 => {
            d.extend(SDF_PAINT);
            d.extend(["HAS_RECT", "PAINT_PATTERN", "CLAMP_PATTERN"]);
        }
        n if n == Shader::SDF_PATTERN_REPEAT.0 => {
            d.extend(SDF_PAINT);
            d.extend(["HAS_RECT", "HAS_TILE", "PAINT_PATTERN", "REPEAT_PATTERN"]);
        }
        n if n == Shader::SDF_PATTERN_MIRROR_U.0 => {
            d.extend(SDF_PAINT);
            d.extend(["HAS_RECT", "HAS_TILE", "PAINT_PATTERN", "MIRRORU_PATTERN"]);
        }
        n if n == Shader::SDF_PATTERN_MIRROR_V.0 => {
            d.extend(SDF_PAINT);
            d.extend(["HAS_RECT", "HAS_TILE", "PAINT_PATTERN", "MIRRORV_PATTERN"]);
        }
        n if n == Shader::SDF_PATTERN_MIRROR.0 => {
            d.extend(SDF_PAINT);
            d.extend(["HAS_RECT", "HAS_TILE", "PAINT_PATTERN", "MIRROR_PATTERN"]);
        }

        // Needs wgpu's DUAL_SOURCE_BLENDING; Noesis only emits it when
        // `DeviceCaps::subpixel_rendering` is set, which the device leaves off.
        n if n == Shader::SDF_LCD_SOLID.0 => {
            d.insert("HAS_COLOR");
            d.insert("HAS_UV1");
            d.insert("HAS_ST1");
            d.insert("HAS_GLYPHS_TEXTURE");
            d.insert("PAINT_SOLID");
            d.insert("EFFECT_SDF_LCD");
        }

        n if n == Shader::OPACITY_SOLID.0 => {
            d.insert("HAS_COLOR");
            d.insert("HAS_UV1");
            d.insert("HAS_IMAGE_TEXTURE");
            d.insert("PAINT_SOLID");
            d.insert("EFFECT_OPACITY");
        }
        n if n == Shader::OPACITY_LINEAR.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_UV1");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("HAS_IMAGE_TEXTURE");
            d.insert("PAINT_LINEAR");
            d.insert("EFFECT_OPACITY");
        }
        n if n == Shader::OPACITY_RADIAL.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_UV1");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("HAS_IMAGE_TEXTURE");
            d.insert("PAINT_RADIAL");
            d.insert("EFFECT_OPACITY");
        }
        n if n == Shader::OPACITY_PATTERN.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_UV1");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("HAS_IMAGE_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("PAINT_PATTERN_PLAIN");
            d.insert("EFFECT_OPACITY");
        }
        n if n == Shader::OPACITY_PATTERN_CLAMP.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_UV1");
            d.insert("HAS_RECT");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("HAS_IMAGE_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("CLAMP_PATTERN");
            d.insert("EFFECT_OPACITY");
        }
        n if n == Shader::OPACITY_PATTERN_REPEAT.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_UV1");
            d.insert("HAS_RECT");
            d.insert("HAS_TILE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("HAS_IMAGE_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("REPEAT_PATTERN");
            d.insert("EFFECT_OPACITY");
        }
        n if n == Shader::OPACITY_PATTERN_MIRROR_U.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_UV1");
            d.insert("HAS_RECT");
            d.insert("HAS_TILE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("HAS_IMAGE_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("MIRRORU_PATTERN");
            d.insert("EFFECT_OPACITY");
        }
        n if n == Shader::OPACITY_PATTERN_MIRROR_V.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_UV1");
            d.insert("HAS_RECT");
            d.insert("HAS_TILE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("HAS_IMAGE_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("MIRRORV_PATTERN");
            d.insert("EFFECT_OPACITY");
        }
        n if n == Shader::OPACITY_PATTERN_MIRROR.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_UV1");
            d.insert("HAS_RECT");
            d.insert("HAS_TILE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("HAS_IMAGE_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("MIRROR_PATTERN");
            d.insert("EFFECT_OPACITY");
        }

        // No PAINT. The DOWNSAMPLE define (distinct from EFFECT_DOWNSAMPLE)
        // makes the vertex shader spread uv0 +/- uv1 into four tap coords.
        n if n == Shader::DOWNSAMPLE.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_UV1");
            d.insert("DOWNSAMPLE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("EFFECT_DOWNSAMPLE");
        }
        n if n == Shader::UPSAMPLE.0 => {
            d.insert("HAS_COLOR");
            d.insert("HAS_UV0");
            d.insert("HAS_UV1");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("HAS_IMAGE_TEXTURE");
            d.insert("EFFECT_UPSAMPLE");
        }

        // Reads `image` and `shadow` at group(3) plus cbuffer1_ps for the shadow
        // color, offset, and blend factor.
        n if n == Shader::SHADOW.0 => {
            d.insert("HAS_COLOR");
            d.insert("HAS_UV1");
            d.insert("HAS_RECT");
            d.insert("HAS_IMAGE_TEXTURE");
            d.insert("HAS_SHADOW_TEXTURE");
            d.insert("HAS_CBUFFER1_PS");
            d.insert("PAINT_SOLID");
            d.insert("EFFECT_SHADOW");
        }

        n if n == Shader::BLUR.0 => {
            d.insert("HAS_COLOR");
            d.insert("HAS_UV1");
            d.insert("HAS_IMAGE_TEXTURE");
            d.insert("HAS_SHADOW_TEXTURE");
            d.insert("HAS_CBUFFER1_PS");
            d.insert("PAINT_SOLID");
            d.insert("EFFECT_BLUR");
        }

        _ => return None,
    }
    Some(d)
}

/// Defines every SDF paint shares: uv0 for the paint, uv1 and st1 for the
/// glyph atlas.
const SDF_PAINT: [&str; 6] = [
    "HAS_UV0",
    "HAS_UV1",
    "HAS_ST1",
    "HAS_PAINT_TEXTURE",
    "HAS_GLYPHS_TEXTURE",
    "EFFECT_SDF",
];

/// The defines that pick a pattern variant's own texture fetch, which a brush
/// shader replaces.
const PATTERN_FETCH: [&str; 6] = [
    "PAINT_PATTERN_PLAIN",
    "CLAMP_PATTERN",
    "REPEAT_PATTERN",
    "MIRRORU_PATTERN",
    "MIRRORV_PATTERN",
    "MIRROR_PATTERN",
];

/// Returns the WGSL define set for a custom shader of `kind` drawn as
/// `shader`, or `None` when that shader can't run it.
///
/// A brush runs as any pattern paint except the `SDF_LCD_*` ones, in place of
/// its texture fetch; an effect runs as `CUSTOM_EFFECT`. `constants` declares
/// the shader's `Constants` uniform.
#[must_use]
pub fn defines_for_custom(
    shader: Shader,
    kind: PixelShaderKind,
    constants: bool,
) -> Option<HashSet<&'static str>> {
    let mut d = match kind {
        PixelShaderKind::Brush => {
            let mut d = defines_for_shader(shader)?;
            if !d.contains("PAINT_PATTERN") {
                return None;
            }
            for define in PATTERN_FETCH {
                d.remove(define);
            }
            d.insert("CUSTOM_PATTERN");
            d
        }
        // Noesis's custom effects: PosColorTex0RectImagePos geometry, the
        // input in the pattern slot, and the solid paint's alpha as opacity.
        PixelShaderKind::Effect => {
            if shader != Shader::CUSTOM_EFFECT {
                return None;
            }
            HashSet::from([
                "HAS_COLOR",
                "HAS_UV0",
                "HAS_RECT",
                "HAS_IMAGE_POS",
                "HAS_PAINT_TEXTURE",
                "PAINT_SOLID",
                "EFFECT_CUSTOM",
            ])
        }
    };
    d.insert("CUSTOM_SHADER");
    if constants {
        d.insert("CUSTOM_CONSTANTS");
    }
    Some(d)
}
