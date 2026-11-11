//! Recognize filter graphs whose output is the original paint clipped to a
//! finite region. An offset of zero is still a crop, not a removable filter.
use super::{transformed, ConversionError, ConversionResult, Layer};
use crate::image::vector_path_ops::{combine, Operation};
use tiny_skia::{Path, PathBuilder, Transform};

/// usvg 0.44 resolves explicit primitive subregions in objectBoundingBox units
/// against the filter region instead of the object's bounds. That information
/// is gone from the normalized tree. Do not classify such a crop as exact.
pub(super) fn validate_source(source: &str, tree: &usvg::Tree) -> ConversionResult<()> {
    use std::collections::{HashMap, HashSet};
    if tree.filters().is_empty() {
        return Ok(());
    }
    let prepared =
        crate::image::svg_document::prepare_svg(source).map_err(ConversionError::Invalid)?;
    let document = usvg::roxmltree::Document::parse(&prepared)
        .map_err(|error| ConversionError::Invalid(format!("SVG XML 解析失败: {error}")))?;
    let definitions: HashMap<_, _> = document
        .descendants()
        .filter(|node| node.has_tag_name("filter"))
        .filter_map(|node| node.attribute("id").map(|id| (id, node)))
        .collect();
    let mut uncertain = HashSet::new();
    for (&id, &definition) in &definitions {
        let mut chain = Vec::new();
        let mut visited = HashSet::new();
        let mut current = Some(definition);
        while let Some(node) = current {
            if !visited.insert(node.id()) {
                break;
            }
            chain.push(node);
            current = node
                .attribute("href")
                .or_else(|| node.attribute(("http://www.w3.org/1999/xlink", "href")))
                .and_then(|href| href.trim().strip_prefix('#'))
                .and_then(|id| definitions.get(id).copied());
        }
        let primitive_units = chain
            .iter()
            .find_map(|node| node.attribute("primitiveUnits"))
            .unwrap_or("userSpaceOnUse");
        if primitive_units.trim() != "objectBoundingBox" {
            continue;
        }
        let primitives = chain
            .iter()
            .find(|node| node.children().any(|child| child.is_element()));
        if primitives.is_some_and(|node| {
            node.children().any(|child| {
                child.is_element()
                    && ["x", "y", "width", "height"]
                        .iter()
                        .any(|attribute| child.attribute(*attribute).is_some())
            })
        }) {
            uncertain.insert(id);
        }
    }
    if !uncertain.is_empty()
        && tree
            .filters()
            .iter()
            .any(|filter| uncertain.contains(filter.id()) || !definitions.contains_key(filter.id()))
    {
        // Normally the first used instance retains its source id. Later usvg
        // clones get generated ids; if only such clones survive, their source
        // cannot be proved safe. Preserve the strict boundary in that case.
        return Err(ConversionError::Effect(
            "使用 objectBoundingBox 子区域的滤镜（当前解析器无法可靠保留其裁剪范围）",
        ));
    }
    Ok(())
}

type Geometry = Option<(Path, usvg::FillRule)>;

fn intersect(left: &Geometry, right: &Geometry) -> ConversionResult<Geometry> {
    combine(
        left.as_ref().map(|(path, rule)| (path, *rule)),
        right.as_ref().map(|(path, rule)| (path, *rule)),
        Operation::Intersect,
    )
    .map_err(ConversionError::Invalid)
}

fn rectangle(rect: usvg::NonZeroRect, transform: Transform) -> ConversionResult<Geometry> {
    Ok(Some((
        transformed(PathBuilder::from_rect(rect.to_rect()), transform)?,
        usvg::FillRule::NonZero,
    )))
}

// None inside a supported result means that its crop is empty. Keep this
// distinct from an unsupported filter operation and from a group with no filter.
#[derive(Clone)]
enum Output {
    Original(Geometry),
    Unsupported(&'static str),
}

pub(super) struct FilterRegion(Geometry);

impl FilterRegion {
    pub(super) fn apply(&self, layers: &mut Vec<Layer>, start: usize) -> ConversionResult<()> {
        let original: Vec<_> = layers.drain(start..).collect();
        for mut layer in original {
            let clipped = combine(
                Some((&layer.path, layer.rule)),
                self.0.as_ref().map(|(path, rule)| (path, *rule)),
                Operation::Intersect,
            )
            .map_err(ConversionError::Invalid)?;
            if let Some((path, rule)) = clipped {
                layer.path = path;
                layer.rule = rule;
                layers.push(layer);
            }
        }
        Ok(())
    }
}

/// Validate before collecting paints, so a real filter effect is not mistaken
/// for an unsupported paint. Apply the returned region before mask/opacity.
/// The last primitive is the output; unreferenced branches do not contribute.
pub(super) fn region(
    group: &usvg::Group,
    transform: Transform,
) -> ConversionResult<Option<FilterRegion>> {
    let mut accumulated: Option<Geometry> = None;
    for filter in group.filters() {
        let filter_region = rectangle(filter.rect(), transform)?;
        let mut results: Vec<(&str, Output)> = Vec::new();
        for primitive in filter.primitives() {
            let output = match primitive.kind() {
                usvg::filter::Kind::Offset(offset) if offset.dx() == 0.0 && offset.dy() == 0.0 => {
                    let input = match offset.input() {
                        usvg::filter::Input::SourceGraphic => {
                            Output::Original(filter_region.clone())
                        }
                        usvg::filter::Input::SourceAlpha => {
                            Output::Unsupported("使用 SourceAlpha 改变颜色的滤镜")
                        }
                        usvg::filter::Input::Reference(name) => results
                            .iter()
                            .rev()
                            .find(|(result, _)| result == name)
                            .map(|(_, output)| output.clone())
                            .unwrap_or(Output::Unsupported("无法解析输入的滤镜")),
                    };
                    match input {
                        Output::Original(input_region) => {
                            // feOffset does not interpolate or combine colors;
                            // its color-interpolation-filters does not alter
                            // the mathematical identity at dx = dy = 0.
                            let primitive_region = rectangle(primitive.rect(), transform)?;
                            Output::Original(intersect(&input_region, &primitive_region)?)
                        }
                        other => other,
                    }
                }
                _ => Output::Unsupported("滤镜"),
            };
            results.push((primitive.result(), output));
        }
        let output = match results.pop().map(|(_, output)| output) {
            Some(Output::Original(region)) => region,
            Some(Output::Unsupported(effect)) => return Err(ConversionError::Effect(effect)),
            None => return Err(ConversionError::Effect("无输出的滤镜")),
        };
        // Multiple filters run sequentially: SourceGraphic in the next filter
        // refers to the previous filter's result, not the unfiltered group.
        accumulated = Some(match accumulated {
            Some(previous) => intersect(&previous, &output)?,
            None => output,
        });
    }
    Ok(accumulated.map(FilterRegion))
}
