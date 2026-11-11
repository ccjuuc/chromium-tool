//! Convert binary SVG masks to geometry, independently of the subject's shape.
//! White luminance / opaque alpha paints add coverage; opaque black luminance
//! paints remove it in paint order. Continuous mask coverage requires sampling.
use super::{collect_path, transformed, ConversionError, ConversionResult};
use crate::image::vector_path_ops::{combine, Operation};
use tiny_skia::{Path, PathBuilder, Rect, Transform};

type Geometry = Option<(Path, usvg::FillRule)>;

fn apply(left: &Geometry, right: &Geometry, operation: Operation) -> ConversionResult<Geometry> {
    combine(
        left.as_ref().map(|(path, rule)| (path, *rule)),
        right.as_ref().map(|(path, rule)| (path, *rule)),
        operation,
    )
    .map_err(ConversionError::Invalid)
}

fn rect_geometry(rect: Rect, transform: Transform) -> ConversionResult<Geometry> {
    Ok(Some((
        transformed(PathBuilder::from_rect(rect), transform)?,
        usvg::FillRule::NonZero,
    )))
}

// Clip paths are a union of filled silhouettes, independent of their paint
// colors or opacity. usvg has already resolved clip-rule into each path's fill.
fn clip_geometry(clip: &usvg::ClipPath, transform: Transform) -> ConversionResult<Geometry> {
    fn visit(group: &usvg::Group, transform: Transform) -> ConversionResult<Geometry> {
        let transform = transform.pre_concat(group.transform());
        let mut result = None;
        for node in group.children() {
            let child = match node {
                usvg::Node::Path(path) if path.is_visible() => match path.fill() {
                    Some(fill) => Some((transformed(path.data().clone(), transform)?, fill.rule())),
                    None => None,
                },
                usvg::Node::Path(_) => None,
                usvg::Node::Group(group) => visit(group, transform)?,
                _ => return Err(ConversionError::Effect("包含非路径内容的蒙版裁剪")),
            };
            result = apply(&result, &child, Operation::Union)?;
        }
        if let Some(clip) = group.clip_path() {
            result = apply(
                &result,
                &clip_geometry(clip, transform)?,
                Operation::Intersect,
            )?;
        }
        Ok(result)
    }
    let mut result = visit(clip.root(), transform.pre_concat(clip.transform()))?;
    if let Some(additional) = clip.clip_path() {
        result = apply(
            &result,
            &clip_geometry(additional, transform)?,
            Operation::Intersect,
        )?;
    }
    Ok(result)
}

// Keep both the visible white coverage and the fully opaque painted region.
// An opaque black child erases prior luminance; transparent children do not.
struct BinaryPaint {
    white: Geometry,
    opaque: Geometry,
}

fn mask_group(
    group: &usvg::Group,
    parent: Transform,
    kind: usvg::MaskType,
) -> ConversionResult<BinaryPaint> {
    let mut result = BinaryPaint {
        white: None,
        opaque: None,
    };
    if group.opacity().get() == 0.0 {
        return Ok(result);
    }
    if group.opacity().get() != 1.0
        || !group.filters().is_empty()
        || group.blend_mode() != usvg::BlendMode::Normal
    {
        return Err(ConversionError::Effect("连续透明度、滤镜或混合模式蒙版"));
    }
    let transform = parent.pre_concat(group.transform());
    for node in group.children() {
        let mut paints = Vec::new();
        match node {
            usvg::Node::Group(child) => paints.push(mask_group(child, transform, kind)?),
            usvg::Node::Path(path) => {
                let mut layers = Vec::new();
                // collect_path uses this only as metadata; mask.region is applied
                // geometrically after the complete mask has been composed.
                let unused_clip = Rect::from_xywh(0.0, 0.0, 1.0, 1.0).unwrap();
                collect_path(path, transform, unused_clip, &mut layers)?;
                for layer in layers {
                    if layer.alpha != 1.0 {
                        return Err(ConversionError::Effect("连续透明度蒙版"));
                    }
                    let white =
                        kind == usvg::MaskType::Alpha || layer.color == usvg::Color::white();
                    if !white && layer.color != usvg::Color::black() {
                        return Err(ConversionError::Effect("灰度或彩色亮度蒙版"));
                    }
                    let coverage = Some((layer.path, layer.rule));
                    paints.push(BinaryPaint {
                        white: if white { coverage.clone() } else { None },
                        opaque: coverage,
                    });
                }
            }
            _ => return Err(ConversionError::Effect("非路径蒙版")),
        }
        for paint in paints {
            result.white = apply(&result.white, &paint.opaque, Operation::Difference)?;
            result.white = apply(&result.white, &paint.white, Operation::Union)?;
            result.opaque = apply(&result.opaque, &paint.opaque, Operation::Union)?;
        }
    }
    let mut restrict = |coverage: Geometry| -> ConversionResult<()> {
        result.white = apply(&result.white, &coverage, Operation::Intersect)?;
        result.opaque = apply(&result.opaque, &coverage, Operation::Intersect)?;
        Ok(())
    };
    if let Some(clip) = group.clip_path() {
        restrict(clip_geometry(clip, transform)?)?;
    }
    if let Some(mask) = group.mask() {
        restrict(geometry(mask, transform)?)?;
    }
    Ok(result)
}

pub(super) fn geometry(mask: &usvg::Mask, transform: Transform) -> ConversionResult<Geometry> {
    let mut result = mask_group(mask.root(), transform, mask.kind())?.white;
    result = apply(
        &result,
        &rect_geometry(mask.rect().to_rect(), transform)?,
        Operation::Intersect,
    )?;
    if let Some(additional) = mask.mask() {
        result = apply(
            &result,
            &geometry(additional, transform)?,
            Operation::Intersect,
        )?;
    }
    Ok(result)
}
