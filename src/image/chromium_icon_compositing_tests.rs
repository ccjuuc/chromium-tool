use super::{reverse, try_convert_svg_source_to_chromium_icon, SvgToChromiumIconOptions};

fn svg(contents: &str) -> String {
    format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16">{contents}</svg>"#)
}

fn render(source: &str) -> tiny_skia::Pixmap {
    let tree = usvg::Tree::from_str(source, &usvg::Options::default()).unwrap();
    let mut pixels = tiny_skia::Pixmap::new(128, 128).unwrap();
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(8.0, 8.0),
        &mut pixels.as_mut(),
    );
    pixels
}

fn equivalent(source: &str) -> (String, tiny_skia::Pixmap) {
    let icon =
        try_convert_svg_source_to_chromium_icon(source, &SvgToChromiumIconOptions::default())
            .unwrap_or_else(|error| panic!("group opacity conversion failed: {error}\n{source}"));
    assert!(!icon.contains("Approximation requested") && !icon.contains("Sampled at"));
    let actual = render(&reverse::convert(&icon, None).unwrap());
    let expected = render(source);
    let total: u64 = expected
        .data()
        .iter()
        .zip(actual.data())
        .map(|(a, b)| u64::from(a.abs_diff(*b)))
        .sum();
    let mean = total as f64 / expected.data().len() as f64;
    let changed = expected
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
    // Compare independent SVG rendering while allowing bounded differences at
    // shared antialiased vector edges versus SVG's offscreen group compositing.
    assert!(
        mean < 1.5 && changed < 300,
        "group rendering mismatch: mean {mean:.3}; {changed} significant pixels\n{source}"
    );
    (icon, actual)
}

fn pixel(image: &tiny_skia::Pixmap, x: u32, y: u32) -> tiny_skia::ColorU8 {
    image.pixel(x * 8 + 4, y * 8 + 4).unwrap().demultiply()
}

#[test]
fn opaque_paints_receive_group_opacity_once_at_overlaps() {
    let source = svg(
        r#"<g opacity=".5"><rect x="1" y="1" width="10" height="10" fill="red"/><rect x="5" y="5" width="10" height="10" fill="blue"/><rect x="7" y="7" width="3" height="3" fill="lime"/></g>"#,
    );
    let (_, image) = equivalent(&source);
    for (x, y, expected) in [
        (2, 2, [255, 0, 0, 128]),
        (6, 6, [0, 0, 255, 128]),
        (8, 8, [0, 255, 0, 128]),
        (12, 12, [0, 0, 255, 128]),
    ] {
        let actual = pixel(&image, x, y);
        assert_eq!(
            [actual.red(), actual.green(), actual.blue(), actual.alpha()],
            expected
        );
    }
}

#[test]
fn opacity_flattening_keeps_external_background_and_compound_holes() {
    let (_, image) = equivalent(&svg(
        r#"<rect width="16" height="16" fill="lime"/><g opacity=".5"><path d="M1 1H15V15H1Z M4 4H12V12H4Z" fill="red" fill-rule="evenodd"/><path d="M8 2C16 2 16 14 8 14C0 14 0 2 8 2Z" fill="blue"/></g>"#,
    ));
    assert_eq!(pixel(&image, 0, 0).green(), 255);
    assert_eq!(pixel(&image, 8, 8).alpha(), 255);
    assert_eq!(pixel(&image, 8, 8).blue(), 128);
    assert_eq!(pixel(&image, 8, 8).green(), 127);
}

#[test]
fn clipped_and_transformed_paints_occlude_only_their_visible_region() {
    let (_, image) = equivalent(&svg(
        r#"<defs><clipPath id="c"><rect x="6" y="4" width="5" height="5"/></clipPath></defs><g opacity=".5" transform="translate(1 1) scale(.9)"><rect x="1" y="1" width="14" height="14" fill="red"/><rect width="16" height="16" fill="blue" clip-path="url(#c)"/></g>"#,
    ));
    assert_eq!(pixel(&image, 3, 3).red(), 255);
    assert_eq!(pixel(&image, 8, 6).blue(), 255);
    assert_eq!(pixel(&image, 12, 6).red(), 255);
    assert_eq!(pixel(&image, 8, 6).alpha(), 128);
}

#[test]
fn template_group_unifies_same_paint_and_preserves_source_opacity() {
    let source = svg(
        r#"<g opacity=".5"><rect x="1" y="1" width="10" height="10" fill="red"/><rect x="5" y="5" width="10" height="10" fill="blue"/></g>"#,
    );
    let icon = try_convert_svg_source_to_chromium_icon(
        &source,
        &SvgToChromiumIconOptions {
            emit_path_colors: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!icon.contains("PATH_COLOR_ARGB"));
    assert!(
        !icon.contains("NEW_PATH"),
        "same template paint should merge"
    );
    let image = render(&reverse::convert_with_color(&icon, None, [200, 60, 20, 64]).unwrap());
    assert_eq!(pixel(&image, 6, 6).alpha(), 128);
    assert_eq!(pixel(&image, 2, 2).alpha(), 128);
}

#[test]
fn nested_disjoint_translucent_regions_can_inherit_opacity() {
    let (_, image) = equivalent(&svg(
        r#"<g opacity=".5"><g opacity=".5"><rect x="1" y="1" width="10" height="10" fill="red"/><rect x="5" y="5" width="10" height="10" fill="blue"/></g></g>"#,
    ));
    assert_eq!(pixel(&image, 2, 2).alpha(), 64);
    assert_eq!(pixel(&image, 6, 6).alpha(), 64);
    equivalent(&svg(
        r#"<g opacity=".5"><rect x="1" y="1" width="5" height="5" fill="red" fill-opacity=".5"/><rect x="9" y="9" width="5" height="5" fill="blue" fill-opacity=".25"/></g>"#,
    ));
}

#[test]
fn translucent_overlaps_require_explicit_approximation() {
    for contents in [
        r#"<rect x="1" y="1" width="10" height="10" fill="red"/><rect x="5" y="5" width="10" height="10" fill="blue" fill-opacity=".5"/>"#,
        r#"<rect x="1" y="1" width="10" height="10" fill="red" fill-opacity=".5"/><rect x="5" y="5" width="10" height="10" fill="blue" fill-opacity=".5"/>"#,
    ] {
        let source = svg(&format!(r#"<g opacity=".5">{contents}</g>"#));
        let error =
            try_convert_svg_source_to_chromium_icon(&source, &SvgToChromiumIconOptions::default())
                .unwrap_err();
        assert!(error.contains("半透明图层重叠"), "{error}");
    }
}

#[test]
fn opaque_top_paint_can_hide_translucent_lower_content() {
    let (_, image) = equivalent(&svg(
        r#"<g opacity=".5"><rect x="2" y="2" width="12" height="12" fill="red" fill-opacity=".4"/><rect x="1" y="1" width="14" height="14" fill="blue"/></g>"#,
    ));
    assert_eq!(pixel(&image, 8, 8).blue(), 255);
    assert_eq!(pixel(&image, 8, 8).alpha(), 128);

    let (_, image) = equivalent(&svg(
        r#"<g opacity=".5"><rect x="1" y="1" width="10" height="10" fill="red" fill-opacity=".4"/><rect x="5" y="5" width="10" height="10" fill="blue"/></g>"#,
    ));
    assert_eq!(pixel(&image, 2, 2).alpha(), 51);
    assert_eq!(pixel(&image, 8, 8).blue(), 255);
    assert_eq!(pixel(&image, 8, 8).alpha(), 128);
}
