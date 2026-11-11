//! Resolve group opacity on visible vector regions before emitting independent
//! Chromium paints. Geometric occlusion is safe for opaque paints; overlapping
//! translucent paints require an offscreen composite and remain unsupported.
use super::{ConversionError, ConversionResult, Layer};
use crate::image::vector_path_ops::{combine, Operation};
use std::collections::HashMap;
use tiny_skia::{Path, PathBuilder, Rect};

type Geometry = Option<(Path, usvg::FillRule)>;

fn operation(left: &Geometry, right: &Geometry, op: Operation) -> ConversionResult<Geometry> {
    combine(
        left.as_ref().map(|(path, rule)| (path, *rule)),
        right.as_ref().map(|(path, rule)| (path, *rule)),
        op,
    )
    .map_err(ConversionError::Invalid)
}

pub(super) fn apply_group_opacity(
    layers: &mut Vec<Layer>,
    start: usize,
    opacity: f32,
    group_clip: Rect,
) -> ConversionResult<()> {
    if opacity == 0.0 {
        layers.truncate(start);
        return Ok(());
    }
    if opacity == 1.0 || layers.len() - start <= 1 {
        for layer in &mut layers[start..] {
            layer.alpha *= opacity;
        }
        return Ok(());
    }

    let mut opaque_above = None;
    let mut translucent_above = None;
    let mut visible_layers = Vec::new();
    for mut layer in layers.drain(start..).rev() {
        if layer.alpha == 0.0 {
            continue;
        }
        // The coverage of a clipped paint must never hide content outside its
        // clip. Bake every local rectangle into geometry before occlusion.
        let geometry = operation(
            &Some((layer.path, layer.rule)),
            &Some((PathBuilder::from_rect(layer.clip), usvg::FillRule::NonZero)),
            Operation::Intersect,
        )?;
        let visible = operation(&geometry, &opaque_above, Operation::Difference)?;
        if visible.is_none() {
            continue;
        }
        if operation(&visible, &translucent_above, Operation::Intersect)?.is_some() {
            return Err(ConversionError::Effect("含半透明图层重叠的组透明度"));
        }
        if layer.alpha == 1.0 {
            opaque_above = operation(&opaque_above, &visible, Operation::Union)?;
        } else {
            // Nonoverlapping translucent content, including disjoint regions
            // produced by nested groups, can safely inherit group opacity.
            translucent_above = operation(&translucent_above, &visible, Operation::Union)?;
        }
        let (path, rule) = visible.unwrap();
        layer.path = path;
        layer.rule = rule;
        layer.clip = group_clip;
        visible_layers.push(layer);
    }
    visible_layers.reverse();

    // Adjacent regions of the same paint can share one contour. This avoids
    // internal antialias seams, especially when all paints use a template color.
    // Distinct-color shared edges can still differ from offscreen rasterization.
    let mut merged: Vec<Layer> = Vec::new();
    let mut paint_indices = HashMap::new();
    for layer in visible_layers {
        let paint = (
            layer.color.red,
            layer.color.green,
            layer.color.blue,
            layer.alpha.to_bits(),
            layer.antialias,
        );
        if let Some(&index) = paint_indices.get(&paint) {
            let existing: &mut Layer = &mut merged[index];
            if let Some((path, rule)) = combine(
                Some((&existing.path, existing.rule)),
                Some((&layer.path, layer.rule)),
                Operation::Union,
            )
            .map_err(ConversionError::Invalid)?
            {
                existing.path = path;
                existing.rule = rule;
            }
        } else {
            paint_indices.insert(paint, merged.len());
            merged.push(layer);
        }
    }
    for layer in &mut merged {
        layer.alpha *= opacity;
    }
    layers.extend(merged);
    Ok(())
}
