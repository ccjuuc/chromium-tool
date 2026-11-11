//! Filled-curve boolean operations through Skia PathOps.
//!
//! Keep geometry and fill rules separate from SVG paint/compositing. No icon
//! shapes are recognised here, and curves are never flattened to polygons.
use tiny_skia::{Path, PathBuilder, PathSegment};
use usvg::FillRule;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Operation {
    Intersect,
    Union,
    Difference,
}

/// `None` is the empty set, both for operands and for a successful empty result.
pub(crate) fn combine(
    left: Option<(&Path, FillRule)>,
    right: Option<(&Path, FillRule)>,
    operation: Operation,
) -> Result<Option<(Path, FillRule)>, String> {
    let left = to_skia(left)?;
    let right = to_skia(right)?;
    let operation = match operation {
        Operation::Intersect => skia_safe::PathOp::Intersect,
        Operation::Union => skia_safe::PathOp::Union,
        Operation::Difference => skia_safe::PathOp::Difference,
    };
    let result = left
        .op(&right, operation)
        .ok_or_else(|| "Skia 无法完成矢量路径布尔运算".to_string())?;
    from_skia(&result)
}

fn to_skia(input: Option<(&Path, FillRule)>) -> Result<skia_safe::Path, String> {
    let Some((path, rule)) = input else {
        return Ok(skia_safe::Path::new());
    };
    let mut builder = skia_safe::PathBuilder::new();
    builder.set_fill_type(match rule {
        FillRule::NonZero => skia_safe::PathFillType::Winding,
        FillRule::EvenOdd => skia_safe::PathFillType::EvenOdd,
    });
    for segment in path.segments() {
        match segment {
            PathSegment::MoveTo(p) => {
                builder.move_to((p.x, p.y));
            }
            PathSegment::LineTo(p) => {
                builder.line_to((p.x, p.y));
            }
            PathSegment::QuadTo(a, b) => {
                builder.quad_to((a.x, a.y), (b.x, b.y));
            }
            PathSegment::CubicTo(a, b, c) => {
                builder.cubic_to((a.x, a.y), (b.x, b.y), (c.x, c.y));
            }
            PathSegment::Close => {
                builder.close();
            }
        }
    }
    if !builder.is_finite() {
        return Err("矢量路径包含非有限坐标".into());
    }
    Ok(builder.detach(None))
}

fn from_skia(path: &skia_safe::Path) -> Result<Option<(Path, FillRule)>, String> {
    if !path.is_finite() {
        return Err("Skia 路径运算产生了非有限坐标".into());
    }
    let rule = match path.fill_type() {
        skia_safe::PathFillType::Winding => FillRule::NonZero,
        skia_safe::PathFillType::EvenOdd => FillRule::EvenOdd,
        _ => return Err("不支持无限反向填充的矢量路径".into()),
    };
    if path.is_empty() {
        return Ok(None);
    }
    let mut builder = PathBuilder::new();
    for segment in path.iter() {
        let points = segment.points();
        match segment.verb() {
            skia_safe::PathVerb::Move => builder.move_to(points[0].x, points[0].y),
            skia_safe::PathVerb::Line => builder.line_to(points[1].x, points[1].y),
            skia_safe::PathVerb::Quad => {
                builder.quad_to(points[1].x, points[1].y, points[2].x, points[2].y);
            }
            skia_safe::PathVerb::Cubic => {
                builder.cubic_to(
                    points[1].x,
                    points[1].y,
                    points[2].x,
                    points[2].y,
                    points[3].x,
                    points[3].y,
                );
            }
            skia_safe::PathVerb::Close => builder.close(),
            skia_safe::PathVerb::Conic => {
                // All operands are polynomial curves. Do not silently flatten
                // a rational conic should a future PathOps version produce one.
                return Err("Skia 路径运算返回了无法直接编码的有理二次曲线".into());
            }
        }
    }
    builder
        .finish()
        .map(|path| Some((path, rule)))
        .ok_or_else(|| "Skia 路径运算返回了无效路径".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rectangle(x: f32, y: f32, w: f32, h: f32) -> Path {
        PathBuilder::from_rect(tiny_skia::Rect::from_xywh(x, y, w, h).unwrap())
    }

    fn pixels(shape: Option<(&Path, FillRule)>) -> Vec<u8> {
        let mut mask = tiny_skia::Mask::new(32, 32).unwrap();
        if let Some((path, rule)) = shape {
            mask.fill_path(
                path,
                match rule {
                    FillRule::NonZero => tiny_skia::FillRule::Winding,
                    FillRule::EvenOdd => tiny_skia::FillRule::EvenOdd,
                },
                false,
                tiny_skia::Transform::identity(),
            );
        }
        mask.data().to_vec()
    }

    fn assert_operation(a: (&Path, FillRule), b: (&Path, FillRule), operation: Operation) {
        let expected: Vec<u8> = pixels(Some(a))
            .into_iter()
            .zip(pixels(Some(b)))
            .map(|(a, b)| match operation {
                Operation::Intersect => a & b,
                Operation::Union => a | b,
                Operation::Difference => a & !b,
            })
            .collect();
        let result = combine(Some(a), Some(b), operation).unwrap();
        assert_eq!(
            pixels(result.as_ref().map(|(path, rule)| (path, *rule))),
            expected
        );
    }

    #[test]
    fn intersect_union_and_difference_follow_filled_regions() {
        let a = rectangle(2.0, 2.0, 20.0, 20.0);
        let b = rectangle(10.0, 10.0, 20.0, 20.0);
        for operation in [
            Operation::Intersect,
            Operation::Union,
            Operation::Difference,
        ] {
            assert_operation((&a, FillRule::NonZero), (&b, FillRule::NonZero), operation);
        }
    }

    #[test]
    fn input_fill_rules_are_preserved_for_nested_contours() {
        let outer = rectangle(2.0, 2.0, 28.0, 28.0);
        let inner = rectangle(10.0, 10.0, 12.0, 12.0);
        let mut builder = PathBuilder::new();
        builder.push_path(&outer);
        builder.push_path(&inner);
        let compound = builder.finish().unwrap();
        let full = rectangle(0.0, 0.0, 32.0, 32.0);
        for rule in [FillRule::NonZero, FillRule::EvenOdd] {
            assert_operation(
                (&compound, rule),
                (&full, FillRule::NonZero),
                Operation::Intersect,
            );
        }
    }

    #[test]
    fn self_intersections_and_reversed_winding_are_geometric() {
        let mut builder = PathBuilder::new();
        builder.move_to(2.0, 2.0);
        builder.line_to(29.0, 28.0);
        builder.line_to(2.0, 28.0);
        builder.line_to(29.0, 2.0);
        builder.close();
        let crossed = builder.finish().unwrap();
        let mut builder = PathBuilder::new();
        builder.move_to(8.0, 4.0);
        builder.line_to(8.0, 30.0);
        builder.line_to(24.0, 30.0);
        builder.line_to(24.0, 4.0);
        builder.close();
        let reversed = builder.finish().unwrap();
        for rule in [FillRule::NonZero, FillRule::EvenOdd] {
            assert_operation(
                (&crossed, rule),
                (&reversed, FillRule::NonZero),
                Operation::Intersect,
            );
        }
    }

    #[test]
    fn empty_intersections_and_empty_operands_are_successful() {
        let a = rectangle(1.0, 1.0, 4.0, 4.0);
        let b = rectangle(12.0, 12.0, 4.0, 4.0);
        assert!(combine(
            Some((&a, FillRule::NonZero)),
            Some((&b, FillRule::NonZero)),
            Operation::Intersect
        )
        .unwrap()
        .is_none());
        assert!(
            combine(None, Some((&a, FillRule::NonZero)), Operation::Intersect)
                .unwrap()
                .is_none()
        );
        assert!(
            combine(None, Some((&a, FillRule::NonZero)), Operation::Difference)
                .unwrap()
                .is_none()
        );
        assert!(combine(None, None, Operation::Union).unwrap().is_none());
        for operation in [Operation::Union, Operation::Difference] {
            let result = combine(Some((&a, FillRule::EvenOdd)), None, operation).unwrap();
            assert_eq!(
                pixels(result.as_ref().map(|(path, rule)| (path, *rule))),
                pixels(Some((&a, FillRule::EvenOdd)))
            );
        }
    }

    #[test]
    fn polynomial_curves_roundtrip_without_flattening() {
        let mut builder = PathBuilder::new();
        builder.move_to(2.0, 16.0);
        builder.quad_to(8.0, 2.0, 16.0, 12.0);
        builder.cubic_to(23.0, 1.0, 31.0, 28.0, 20.0, 28.0);
        builder.close();
        let source = builder.finish().unwrap();
        let outer = rectangle(0.0, 0.0, 32.0, 32.0);
        assert_operation(
            (&source, FillRule::NonZero),
            (&outer, FillRule::NonZero),
            Operation::Intersect,
        );
        let (result, _) = combine(
            Some((&source, FillRule::NonZero)),
            Some((&outer, FillRule::NonZero)),
            Operation::Intersect,
        )
        .unwrap()
        .unwrap();
        assert!(result
            .segments()
            .any(|s| matches!(s, PathSegment::QuadTo(..) | PathSegment::CubicTo(..))));
    }

    #[test]
    fn unsupported_inverse_fill_and_rational_conics_are_explicit_errors() {
        let inverse =
            skia_safe::Path::new().with_fill_type(skia_safe::PathFillType::InverseWinding);
        assert!(from_skia(&inverse).is_err());
        let mut conic = skia_safe::PathBuilder::new();
        conic
            .move_to((0.0, 0.0))
            .conic_to((1.0, 2.0), (2.0, 0.0), 0.5)
            .close();
        assert!(from_skia(&conic.detach(None)).is_err());
    }
}
