use super::*;

fn svg(body: &str) -> String {
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16">{body}</svg>"#
    )
}

fn render(source: &str) -> tiny_skia::Pixmap {
    let tree = crate::image::svg_document::parse_svg(source).unwrap();
    let width = tree.size().width();
    let height = tree.size().height();
    let canvas = width.max(height).ceil();
    let scale = 128.0 / canvas;
    let mut image = tiny_skia::Pixmap::new(128, 128).unwrap();
    let transform = Transform::from_row(
        scale,
        0.0,
        0.0,
        scale,
        (canvas - width) * scale / 2.0,
        (canvas - height) * scale / 2.0,
    );
    resvg::render(&tree, transform, &mut image.as_mut());
    let viewport =
        tiny_skia::PathBuilder::from_rect(Rect::from_xywh(0.0, 0.0, width, height).unwrap());
    let mut mask = tiny_skia::Mask::new(128, 128).unwrap();
    mask.fill_path(&viewport, tiny_skia::FillRule::Winding, true, transform);
    image.apply_mask(&mask);
    image
}

fn roundtrip(source: &str, options: SvgToChromiumIconOptions) -> (String, tiny_skia::Pixmap) {
    let icon = try_convert_svg_source_to_chromium_icon(source, &options).unwrap();
    let markup = try_convert_chromium_icon_source_to_svg_markup(&icon).unwrap();
    (icon, render(&markup))
}

fn assert_equivalent(source: &str) -> (String, tiny_skia::Pixmap) {
    let expected = render(source);
    let (icon, actual) = roundtrip(source, SvgToChromiumIconOptions::default());
    let difference: u64 = expected
        .data()
        .iter()
        .zip(actual.data())
        .map(|(&a, &b)| u64::from(a.abs_diff(b)))
        .sum();
    let mean = difference as f64 / expected.data().len() as f64;
    let changed_pixels = expected
        .pixels()
        .iter()
        .zip(actual.pixels())
        .filter(|(a, b)| {
            a.red()
                .abs_diff(b.red())
                .max(a.green().abs_diff(b.green()))
                .max(a.blue().abs_diff(b.blue()))
                .max(a.alpha().abs_diff(b.alpha()))
                > 24
        })
        .count();
    assert!(mean < 1.2 && changed_pixels < 256,
        "rendering mismatch: mean channel error {mean:.3}, {changed_pixels} materially different pixels\n{icon}");
    (icon, actual)
}

#[test]
fn nonzero_and_evenodd_preserve_compound_path_holes() {
    for (rule, center_alpha) in [("", 255), (r#" fill-rule="evenodd""#, 0)] {
        let source = svg(&format!(r#"<path{rule} d="M2 2H14V14H2Z M5 5H11V11H5Z"/>"#));
        let (icon, image) = assert_equivalent(&source);
        assert_eq!(image.pixel(64, 64).unwrap().alpha(), center_alpha);
        assert_eq!(icon.contains("FILL_RULE_NONZERO"), rule.is_empty());
    }
}

#[test]
fn viewbox_css_visibility_and_css_rgba_follow_svg_semantics() {
    let source = r#"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="16" viewBox="10 20 16 8">
<style>.hidden { display:none } .invisible { visibility:hidden } .paint { fill:rgba(255,0,0,1) }</style>
<rect x="10" y="20" width="16" height="8" class="paint"/>
<rect x="10" y="20" width="16" height="8" class="hidden" fill="blue"/>
<rect x="10" y="20" width="16" height="8" class="invisible" fill="green"/>
</svg>"#;
    let (_, image) = assert_equivalent(source);
    assert_eq!(image.pixel(64, 64).unwrap().demultiply().red(), 255);
    assert_eq!(image.pixel(64, 64).unwrap().alpha(), 255);
    assert_eq!(image.pixel(64, 8).unwrap().alpha(), 0);
    assert_eq!(image.pixel(64, 120).unwrap().alpha(), 0);
}

#[test]
fn fill_and_stroke_keep_paint_order_alpha_and_nonuniform_geometry() {
    for paint_order in ["normal", "stroke fill"] {
        let source = svg(&format!(
            r##"<g transform="translate(2 4) scale(1.5 .6)">
<path d="M1 1H7V11H1Z" fill="#e09030" fill-opacity=".7" stroke="#2040b0" stroke-opacity=".8" stroke-width="2" stroke-linejoin="round" paint-order="{paint_order}"/>
<path d="M1 13H7" fill="none" stroke="#20a060" stroke-width="1" stroke-linecap="round" stroke-dasharray="1 1"/>
</g>"##
        ));
        let (icon, _) = assert_equivalent(&source);
        assert!(icon.contains("NEW_PATH"));
        assert!(
            !icon.lines().any(|line| line.starts_with("STROKE,")),
            "affine strokes need expanded outlines"
        );
    }
}

#[test]
fn explicit_quadratic_and_cubic_curves_roundtrip_without_shorthand() {
    let source = svg(
        r##"<path d="M1 8 Q3 1 5 8 T9 8 C10 2 12 2 13 8 S15 14 15 8 L15 15H1Z" fill="#456789"/>"##,
    );
    let (icon, _) = assert_equivalent(&source);
    assert!(icon.contains("QUADRATIC_TO,") || icon.contains("CUBIC_TO,"));
    assert!(!icon.contains("SHORTHAND"));
}

#[test]
fn rectangular_clips_preserve_transforms_and_object_bounding_boxes() {
    for body in [
        r##"<defs><clipPath id="c"><rect x="2" y="3" width="8" height="6"/></clipPath></defs><g transform="translate(1 1) scale(.8 1.2)" clip-path="url(#c)"><rect width="16" height="16" fill="#40a0d0"/></g>"##,
        r##"<defs><clipPath id="c" clipPathUnits="objectBoundingBox"><rect x=".25" y=".25" width=".5" height=".5"/></clipPath></defs><rect x="2" y="4" width="12" height="8" fill="#40a0d0" clip-path="url(#c)"/>"##,
        r##"<defs><clipPath id="c" transform="translate(2 3)"><rect width="8" height="6"/></clipPath></defs><rect width="16" height="16" fill="#40a0d0" clip-path="url(#c)"/>"##,
    ] {
        let (icon, _) = assert_equivalent(&svg(body));
        assert!(icon.contains("CLIP,"));
    }
}

#[test]
fn sun_keeps_eight_rays_in_color_and_template_modes() {
    let source = include_str!("fixtures/light_mode_use.svg");
    let (_, colored) = assert_equivalent(source);
    let (template_icon, template) = roundtrip(
        source,
        SvgToChromiumIconOptions {
            emit_path_colors: false,
            ..Default::default()
        },
    );
    assert!(!template_icon.contains("PATH_COLOR_ARGB"));
    for (x, y) in [
        (64, 8),
        (104, 24),
        (120, 64),
        (104, 104),
        (64, 120),
        (24, 104),
        (8, 64),
        (24, 24),
    ] {
        for image in [&colored, &template] {
            assert!(
                ((y - 4)..=(y + 4)).any(|py| ((x - 4)..=(x + 4)).any(|px| image
                    .pixel(px, py)
                    .unwrap()
                    .alpha()
                    > 128)),
                "missing ray at ({x}, {y})"
            );
        }
    }
    assert_eq!(colored.pixel(64, 64).unwrap().alpha(), 0);
    assert_eq!(template.pixel(64, 64).unwrap().alpha(), 0);
    assert_eq!(
        colored
            .data()
            .chunks_exact(4)
            .map(|p| p[3])
            .collect::<Vec<_>>(),
        template
            .data()
            .chunks_exact(4)
            .map(|p| p[3])
            .collect::<Vec<_>>()
    );
}

#[test]
fn unsupported_effects_require_explicit_approximation() {
    for body in [
        r##"<defs><clipPath id="c"><circle cx="8" cy="8" r="5"/></clipPath></defs><rect width="16" height="16" fill="red" clip-path="url(#c)"/>"##,
        r##"<defs><linearGradient id="g"><stop stop-color="red"/><stop offset="1" stop-color="blue"/></linearGradient></defs><rect x="2" y="2" width="12" height="12" fill="url(#g)"/>"##,
        r##"<defs><mask id="m"><rect width="16" height="16" fill="white"/><circle cx="8" cy="8" r="3" fill="black"/></mask></defs><rect x="2" y="2" width="12" height="12" fill="red" mask="url(#m)"/>"##,
        r#"<g opacity=".5"><rect x="2" y="2" width="8" height="8" fill="red"/><rect x="6" y="6" width="8" height="8" fill="blue"/></g>"#,
    ] {
        let source = svg(body);
        let error =
            try_convert_svg_source_to_chromium_icon(&source, &Default::default()).unwrap_err();
        assert!(error.contains("近似"), "unexpected error: {error}");
        let (icon, actual) = roundtrip(
            &source,
            SvgToChromiumIconOptions {
                approximate_effects: true,
                ..Default::default()
            },
        );
        assert!(icon.contains("Approximation requested"));
        assert!(actual.pixels().iter().any(|p| p.alpha() > 0));
        let expected = render(&source);
        // Approximation can simplify boundaries, but must preserve the image's
        // overall color and compositing rather than dropping the effect.
        let difference: u64 = actual
            .data()
            .iter()
            .zip(expected.data())
            .map(|(&a, &b)| u64::from(a.abs_diff(b)))
            .sum();
        assert!(difference as f64 / (actual.data().len() as f64) < 12.0);
    }
}

#[test]
fn white_shapes_and_transparent_documents_are_preserved() {
    let (_, white) = assert_equivalent(&svg(
        r#"<rect x="2" y="2" width="12" height="12" fill="white"/>"#,
    ));
    assert_eq!(white.pixel(64, 64).unwrap().demultiply().red(), 255);
    assert_eq!(white.pixel(64, 64).unwrap().alpha(), 255);
    let (_, transparent) = assert_equivalent(&svg(
        r#"<rect width="16" height="16" opacity="0"/><path d="M0 0H16V16Z" fill="none"/>"#,
    ));
    assert!(transparent.pixels().iter().all(|p| p.alpha() == 0));
}

#[test]
fn failed_conversion_preserves_existing_output_and_source() {
    let directory =
        std::env::temp_dir().join(format!("chromium_forward_atomic_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("source.svg");
    let output = directory.join("result.icon");
    let invalid = "<svg>";
    std::fs::write(&source, invalid).unwrap();
    std::fs::write(&output, b"previous output").unwrap();
    assert!(try_convert_svg_to_chromium_icon(source.to_str().unwrap(), "result.icon").is_err());
    assert_eq!(std::fs::read(&output).unwrap(), b"previous output");
    assert!(try_convert_svg_to_chromium_icon(source.to_str().unwrap(), "source.svg").is_err());
    assert_eq!(std::fs::read_to_string(&source).unwrap(), invalid);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn decimal_output_preserves_f32_precision() {
    for value in [0.000_000_1f32, 1.234_567_8, -123.456_79, 16_383.999] {
        assert_eq!(format_number(value).parse::<f32>().unwrap(), value);
    }
    assert_eq!(format_number(-0.0), "0");
}

#[test]
fn retraced_clip_is_not_mistaken_for_a_rectangle() {
    let source = svg(
        r##"<defs><clipPath id="c"><path d="M0 0H8H0V8Z"/></clipPath></defs><rect width="16" height="16" fill="red" clip-path="url(#c)"/>"##,
    );
    assert!(try_convert_svg_source_to_chromium_icon(&source, &Default::default()).is_err());
    let (_, image) = roundtrip(
        &source,
        SvgToChromiumIconOptions {
            approximate_effects: true,
            ..Default::default()
        },
    );
    assert!(image.pixels().iter().all(|pixel| pixel.alpha() == 0));
}

#[test]
fn stroke_expansion_accounts_for_large_transforms() {
    assert_equivalent(&svg(
        r#"<g transform="scale(1000)"><circle cx=".008" cy=".008" r=".005" fill="none" stroke="red" stroke-width=".001"/></g>"#,
    ));
}

#[test]
fn approximation_preserves_template_alpha_and_blurred_masks() {
    let foreign_object = include_str!("fixtures/raw_masked_blur.svg");
    assert!(try_convert_svg_source_to_chromium_icon(
        foreign_object,
        &SvgToChromiumIconOptions {
            approximate_effects: true,
            ..Default::default()
        }
    )
    .unwrap_err()
    .contains("foreignObject"));
    let source = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16">
      <defs><filter id="b" x="-100%" y="-100%" width="300%" height="300%"><feGaussianBlur stdDeviation="1"/></filter>
      <mask id="m"><rect width="16" height="16" fill="white"/><circle cx="8" cy="8" r="2" fill="black"/></mask></defs>
      <g mask="url(#m)"><rect x="3" y="3" width="10" height="10" fill="red" opacity=".7" filter="url(#b)"/></g>
    </svg>"##;
    assert!(try_convert_svg_source_to_chromium_icon(source, &Default::default()).is_err());
    let (_, colored) = roundtrip(
        source,
        SvgToChromiumIconOptions {
            approximate_effects: true,
            ..Default::default()
        },
    );
    let (icon, template) = roundtrip(
        source,
        SvgToChromiumIconOptions {
            approximate_effects: true,
            emit_path_colors: false,
        },
    );
    assert!(!icon.contains("PATH_COLOR_ARGB"));
    assert!(template
        .pixels()
        .iter()
        .any(|p| p.alpha() > 0 && p.alpha() < 240));
    let alpha_difference: u64 = colored
        .pixels()
        .iter()
        .zip(template.pixels())
        .map(|(a, b)| u64::from(a.alpha().abs_diff(b.alpha())))
        .sum();
    assert!(alpha_difference as f64 / colored.pixels().len() as f64 <= 1.0);
}
