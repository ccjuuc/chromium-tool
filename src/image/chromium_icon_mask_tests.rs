use super::{reverse, try_convert_svg_source_to_chromium_icon, SvgToChromiumIconOptions};

const SUBJECT_COLOR: [u8; 4] = [0x18, 0x5c, 0xa8, 255];

fn svg(body: &str) -> String {
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16">{body}</svg>"#
    )
}

fn masked(mask_attributes: &str, mask_contents: &str, subject: &str) -> String {
    svg(&format!(
        r#"<defs><mask id="m" maskUnits="userSpaceOnUse" x="0" y="0" width="16" height="16" {mask_attributes}>{mask_contents}</mask></defs>{subject}"#
    ))
}

fn full_subject() -> &'static str {
    r##"<rect width="16" height="16" fill="#185ca8" mask="url(#m)"/>"##
}

// Parse the reference with usvg directly, independently of the converter's
// normalization and path extraction. All fixtures use the same square viewport.
fn render(source: &str) -> tiny_skia::Pixmap {
    let tree = usvg::Tree::from_str(source, &usvg::Options::default()).unwrap();
    let mut image = tiny_skia::Pixmap::new(128, 128).unwrap();
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(8.0, 8.0),
        &mut image.as_mut(),
    );
    image
}

fn assert_pixels_match(expected: &tiny_skia::Pixmap, actual: &tiny_skia::Pixmap, context: &str) {
    let error: u64 = expected
        .data()
        .iter()
        .zip(actual.data())
        .map(|(&a, &b)| u64::from(a.abs_diff(b)))
        .sum();
    let mean = error as f64 / expected.data().len() as f64;
    let materially_changed = expected
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
    // Resvg mask compositing and direct geometric edges can differ in their
    // antialiasing. Bound both aggregate error and significant pixel changes.
    assert!(
        mean < 1.5 && materially_changed < 300,
        "{context}: mean channel error {mean:.3}; {materially_changed} significant pixels"
    );
}

fn equivalent(source: &str) -> (String, tiny_skia::Pixmap) {
    let icon =
        try_convert_svg_source_to_chromium_icon(source, &SvgToChromiumIconOptions::default())
            .unwrap_or_else(|error| panic!("strict mask conversion failed: {error}\n{source}"));
    assert!(!icon.contains("Approximation requested") && !icon.contains("Sampled at"));
    let markup = reverse::convert_with_color(&icon, None, SUBJECT_COLOR).unwrap();
    let actual = render(&markup);
    assert_pixels_match(&render(source), &actual, source);
    (icon, actual)
}

fn pixel(image: &tiny_skia::Pixmap, x: u32, y: u32) -> tiny_skia::ColorU8 {
    image.pixel(x * 8 + 4, y * 8 + 4).unwrap().demultiply()
}

#[test]
fn binary_masks_preserve_triangles_curves_and_compound_holes() {
    for geometry in [
        r#"<path d="M2 14L8 2L14 14Z" fill="white"/>"#,
        r#"<path d="M2 8C2 1 14 1 14 8C14 15 2 15 2 8Z" fill="white"/>"#,
        r#"<path d="M1 1H15V15H1Z M5 5H11V11H5Z" fill="white" fill-rule="evenodd"/>"#,
        r#"<path d="M1 1H15V15H1Z M5 5V11H11V5Z" fill="white" fill-rule="nonzero"/>"#,
    ] {
        let (_, image) = equivalent(&masked("", geometry, full_subject()));
        assert_eq!(pixel(&image, 0, 0).alpha(), 0);
        if geometry.contains("fill-rule") {
            assert_eq!(pixel(&image, 8, 8).alpha(), 0);
            assert_eq!(pixel(&image, 3, 8).alpha(), 255);
        }
    }
}

#[test]
fn luminance_mask_white_black_white_obeys_paint_order() {
    let white = r#"<rect x="1" y="1" width="14" height="14" fill="white"/>"#;
    let black = r#"<rect x="4" y="4" width="8" height="8" fill="black"/>"#;
    let restore = r#"<rect x="7" y="7" width="2" height="2" fill="white"/>"#;
    let (_, cutout) = equivalent(&masked(
        "",
        &format!("{white}{black}{restore}"),
        full_subject(),
    ));
    assert_eq!(pixel(&cutout, 2, 8).alpha(), 255);
    assert_eq!(pixel(&cutout, 5, 8).alpha(), 0);
    assert_eq!(pixel(&cutout, 7, 7).alpha(), 255);

    let (_, repainted) = equivalent(&masked("", &format!("{black}{white}"), full_subject()));
    assert_eq!(pixel(&repainted, 5, 8).alpha(), 255);
}

#[test]
fn alpha_masks_use_coverage_of_any_opaque_color() {
    for color in ["black", "red", "#25b07a"] {
        let geometry = format!(r#"<path d="M2 14L8 2L14 14Z" fill="{color}"/>"#);
        let (_, image) = equivalent(&masked(r#"mask-type="alpha""#, &geometry, full_subject()));
        assert_eq!(pixel(&image, 8, 10).alpha(), 255, "{color}");
        assert_eq!(pixel(&image, 2, 2).alpha(), 0, "{color}");
    }
}

#[test]
fn continuous_mask_coverage_requires_explicit_approximation() {
    let gradient = r#"<defs><linearGradient id="g"><stop stop-color="black"/><stop offset="1" stop-color="white"/></linearGradient></defs><rect width="16" height="16" fill="url(#g)"/>"#;
    for (attributes, geometry) in [
        ("", r##"<rect width="16" height="16" fill="#808080"/>"##),
        (
            "",
            r#"<rect width="16" height="16" fill="white" fill-opacity=".5"/>"#,
        ),
        (
            r#"mask-type="alpha""#,
            r#"<rect width="16" height="16" fill="red" fill-opacity=".5"/>"#,
        ),
        ("", gradient),
    ] {
        let source = masked(attributes, geometry, full_subject());
        let error =
            try_convert_svg_source_to_chromium_icon(&source, &SvgToChromiumIconOptions::default())
                .expect_err("continuous mask coverage must not be silently made binary");
        assert!(error.contains("近似复杂效果"), "{error}");
    }
}

#[test]
fn transparent_mask_paint_does_not_erase_previous_white() {
    let source = masked(
        "",
        r#"<rect x="2" y="2" width="12" height="12" fill="white"/><rect width="16" height="16" fill="black" fill-opacity="0"/>"#,
        full_subject(),
    );
    let (_, image) = equivalent(&source);
    assert_eq!(pixel(&image, 8, 8).alpha(), 255);
}

#[test]
fn mask_region_and_content_units_resolve_against_subject_bounds() {
    let region = svg(
        r##"<defs><mask id="m" maskUnits="objectBoundingBox" x=".25" y=".25" width=".5" height=".5"><rect width="16" height="16" fill="white"/></mask></defs><rect x="2" y="4" width="12" height="8" fill="#185ca8" mask="url(#m)"/>"##,
    );
    let (_, region_image) = equivalent(&region);
    assert_eq!(pixel(&region_image, 8, 8).alpha(), 255);
    assert_eq!(pixel(&region_image, 3, 8).alpha(), 0);
    assert_eq!(pixel(&region_image, 8, 5).alpha(), 0);

    let content = svg(
        r##"<defs><mask id="m" maskUnits="userSpaceOnUse" x="0" y="0" width="16" height="16" maskContentUnits="objectBoundingBox"><g transform="translate(.25 .25)"><path d="M0 0H.5V.5H0Z" fill="white"/></g></mask></defs><g transform="translate(1 2) scale(.75 1)"><rect x="2" y="2" width="12" height="8" fill="#185ca8" mask="url(#m)"/></g>"##,
    );
    let (_, content_image) = equivalent(&content);
    assert_eq!(pixel(&content_image, 6, 7).alpha(), 255);
    assert_eq!(pixel(&content_image, 3, 7).alpha(), 0);
}

#[test]
fn nested_masks_and_nonrectangular_clips_intersect_geometrically() {
    let source = svg(r##"<defs>
<mask id="inner" maskUnits="userSpaceOnUse" x="0" y="0" width="16" height="16"><circle cx="8" cy="8" r="6" fill="white"/></mask>
<clipPath id="triangle"><path d="M2 14L8 2L14 14Z"/></clipPath>
<mask id="m" maskUnits="userSpaceOnUse" x="0" y="0" width="16" height="16"><g mask="url(#inner)" clip-path="url(#triangle)"><rect width="16" height="16" fill="white"/></g></mask>
</defs><rect width="16" height="16" fill="#185ca8" mask="url(#m)"/>"##);
    let (_, image) = equivalent(&source);
    assert_eq!(pixel(&image, 8, 8).alpha(), 255);
    assert_eq!(pixel(&image, 2, 8).alpha(), 0);
    assert_eq!(pixel(&image, 3, 13).alpha(), 0);
}

#[test]
fn masked_holes_reveal_previous_paint_without_clearing_it() {
    let source = masked(
        "",
        r#"<rect x="1" y="1" width="14" height="14" fill="white"/><circle cx="8" cy="8" r="3" fill="black"/>"#,
        r##"<rect width="16" height="16" fill="#e07020"/><rect width="16" height="16" fill="#185ca8" mask="url(#m)"/>"##,
    );
    let (icon, image) = equivalent(&source);
    assert!(!icon.contains("PATH_MODE_CLEAR"));
    let center = pixel(&image, 8, 8);
    assert_eq!(
        [center.red(), center.green(), center.blue(), center.alpha()],
        [0xe0, 0x70, 0x20, 255]
    );
    assert_eq!(pixel(&image, 2, 8).blue(), 0xa8);
}

#[test]
fn disjoint_mask_and_subject_produce_transparent_output() {
    let source = masked(
        "",
        r#"<rect x="10" y="1" width="4" height="14" fill="white"/>"#,
        r##"<rect x="1" y="1" width="4" height="14" fill="#185ca8" mask="url(#m)"/>"##,
    );
    let (_, image) = equivalent(&source);
    assert!(image.pixels().iter().all(|p| p.alpha() == 0));
}

#[test]
fn identical_mask_bounds_do_not_imply_identical_geometry() {
    let (_, rectangle) = equivalent(&masked(
        "",
        r#"<rect x="2" y="2" width="12" height="12" fill="white"/>"#,
        full_subject(),
    ));
    let (_, triangle) = equivalent(&masked(
        "",
        r#"<path d="M2 14L8 2L14 14Z" fill="white"/>"#,
        full_subject(),
    ));
    let (_, ellipse) = equivalent(&masked(
        "",
        r#"<ellipse cx="8" cy="8" rx="6" ry="6" fill="white"/>"#,
        full_subject(),
    ));
    assert_eq!(pixel(&rectangle, 2, 2).alpha(), 255);
    assert_eq!(pixel(&triangle, 2, 2).alpha(), 0);
    assert_eq!(pixel(&ellipse, 2, 2).alpha(), 0);
    assert_eq!(pixel(&ellipse, 3, 7).alpha(), 255);
    assert_eq!(pixel(&triangle, 3, 7).alpha(), 0);
}

#[test]
fn masked_template_changes_runtime_color_and_preserves_source_opacity() {
    let source = masked(
        "",
        r#"<path d="M2 14L8 2L14 14Z" fill="white"/>"#,
        r##"<rect width="16" height="16" fill="#185ca8" fill-opacity=".2" mask="url(#m)"/>"##,
    );
    let icon = try_convert_svg_source_to_chromium_icon(
        &source,
        &SvgToChromiumIconOptions {
            emit_path_colors: false,
            approximate_effects: false,
        },
    )
    .unwrap();
    assert!(!icon.contains("PATH_COLOR_ARGB"));
    for (color, css) in [
        (SUBJECT_COLOR, "#185ca8"),
        ([0xd0, 0x40, 0x20, 255], "#d04020"),
    ] {
        let markup = reverse::convert_with_color(&icon, None, color).unwrap();
        let image = render(&markup);
        let expected = render(&source.replace("#185ca8", css));
        assert_pixels_match(&expected, &image, css);
        assert_eq!(pixel(&image, 8, 10).alpha(), 51);
    }
}

#[test]
fn curved_stroke_intersections_keep_affine_transforms_and_stroke_width() {
    let source = masked(
        "",
        r#"<path d="M1 14L8 1L15 14Z" fill="white"/>"#,
        r##"<g transform="translate(1 2) scale(.8 .7)" mask="url(#m)"><path d="M1 8C1 1 15 1 15 8C15 15 1 15 1 8Z" fill="none" stroke="#185ca8" stroke-width="2" stroke-linejoin="round"/></g>"##,
    );
    let (icon, image) = equivalent(&source);
    assert!(
        !icon.contains("STROKE,"),
        "affine strokes must retain their full outline"
    );
    assert_eq!(pixel(&image, 7, 7).alpha(), 0);
    assert!(image.pixels().iter().filter(|p| p.alpha() > 128).count() > 100);
}
