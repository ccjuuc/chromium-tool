use super::{reverse, try_convert_svg_source_to_chromium_icon, SvgToChromiumIconOptions};

fn svg(body: &str) -> String {
    format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16">{body}</svg>"#)
}

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

fn converted(source: &str) -> (String, tiny_skia::Pixmap) {
    let icon =
        try_convert_svg_source_to_chromium_icon(source, &SvgToChromiumIconOptions::default())
            .unwrap_or_else(|error| panic!("filter conversion failed: {error}\n{source}"));
    assert!(!icon.contains("Approximation requested") && !icon.contains("Sampled at"));
    let actual = render(&reverse::convert(&icon, None).unwrap());
    (icon, actual)
}

fn compare(actual: &tiny_skia::Pixmap, expected: &str) {
    let expected = render(expected);
    let error: u64 = expected
        .data()
        .iter()
        .zip(actual.data())
        .map(|(&a, &b)| u64::from(a.abs_diff(b)))
        .sum();
    let mean = error as f64 / expected.data().len() as f64;
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
    assert!(
        mean < 1.2 && changed < 200,
        "filter geometry mismatch: mean {mean:.3}; {changed} significant pixels"
    );
}

fn pixel(image: &tiny_skia::Pixmap, x: u32, y: u32) -> tiny_skia::ColorU8 {
    image.pixel(x * 8 + 4, y * 8 + 4).unwrap().demultiply()
}

#[test]
fn zero_offsets_preserve_source_color_opacity_and_input_chain() {
    for interpolation in ["sRGB", "linearRGB"] {
        let source = svg(&format!(
            r##"<defs><filter id="f" color-interpolation-filters="{interpolation}">
<feOffset dx="0" dy="0" in="SourceGraphic" result="first"/>
<feOffset in="first" result="second"/><feOffset/></filter></defs>
<g filter="url(#f)"><rect x="1" y="1" width="8" height="8" fill="#385ca8"/>
<circle cx="10" cy="10" r="5" fill="#e08040" fill-opacity=".6"/></g>"##
        ));
        let (_, actual) = converted(&source);
        compare(&actual, &source);
        assert_eq!(
            pixel(&actual, 2, 2),
            tiny_skia::ColorU8::from_rgba(0x38, 0x5c, 0xa8, 255)
        );
    }
}

#[test]
fn filter_region_crop_is_preserved_and_does_not_crop_siblings() {
    let source = svg(
        r##"<defs><filter id="f" filterUnits="userSpaceOnUse" x="4" y="3" width="5" height="7"><feOffset/></filter></defs>
<rect width="16" height="16" fill="#305090"/>
<rect x="1" y="1" width="14" height="14" fill="#e06020" filter="url(#f)"/>"##,
    );
    let (_, actual) = converted(&source);
    compare(&actual, &source);
    assert_eq!(pixel(&actual, 2, 2).red(), 0x30);
    assert_eq!(pixel(&actual, 5, 5).red(), 0xe0);
}

#[test]
fn primitive_regions_follow_references_and_ignore_unreferenced_branches() {
    // resvg 0.44 explicitly omits feOffset primitive-region clipping. Use the
    // independently constructed expected geometry, not that renderer bug.
    let source = svg(
        r##"<defs><filter id="f" filterUnits="userSpaceOnUse" x="0" y="0" width="16" height="16">
<feOffset x="2" y="3" width="10" height="9" result="a"/>
<feGaussianBlur in="SourceGraphic" stdDeviation="3" result="unused"/>
<feOffset in="a" x="6" y="1" width="8" height="7" result="b"/>
<feOffset in="b"/>
</filter></defs><rect width="16" height="16" fill="#4080c0" filter="url(#f)"/>"##,
    );
    let (_, actual) = converted(&source);
    compare(
        &actual,
        &svg(r##"<rect x="6" y="3" width="6" height="5" fill="#4080c0"/>"##),
    );
    assert_eq!(pixel(&actual, 4, 4).alpha(), 0);
    assert_eq!(pixel(&actual, 7, 4).alpha(), 255);
    assert_eq!(pixel(&actual, 7, 10).alpha(), 0);
}

#[test]
fn explicit_sourcegraphic_restarts_filter_input_instead_of_accumulating_branches() {
    let source = svg(
        r##"<defs><filter id="f" filterUnits="userSpaceOnUse" x="0" y="0" width="16" height="16">
<feOffset x="0" y="0" width="2" height="2" result="small"/>
<feOffset in="SourceGraphic" x="5" y="4" width="6" height="7"/>
</filter></defs><rect width="16" height="16" fill="#4080c0" filter="url(#f)"/>"##,
    );
    let (_, actual) = converted(&source);
    compare(
        &actual,
        &svg(r##"<rect x="5" y="4" width="6" height="7" fill="#4080c0"/>"##),
    );
}

#[test]
fn empty_primitive_intersection_stays_transparent() {
    let source = svg(
        r##"<defs><filter id="f" filterUnits="userSpaceOnUse" x="1" y="1" width="4" height="4">
<feOffset x="10" y="10" width="3" height="3"/>
</filter></defs><rect width="16" height="16" fill="red" filter="url(#f)"/>"##,
    );
    let (_, actual) = converted(&source);
    assert!(actual.pixels().iter().all(|pixel| pixel.alpha() == 0));
}

#[test]
fn transformed_filter_and_primitive_regions_keep_their_shape() {
    let source = svg(
        r##"<defs><filter id="f" filterUnits="userSpaceOnUse" x="2" y="2" width="8" height="9">
<feOffset x="4" y="1" width="9" height="7"/>
</filter></defs><g transform="translate(8 0) rotate(30) scale(.8)">
<rect width="16" height="16" fill="#4080c0" filter="url(#f)"/></g>"##,
    );
    let (_, actual) = converted(&source);
    compare(
        &actual,
        &svg(r##"<g transform="translate(8 0) rotate(30) scale(.8)">
<rect x="4" y="2" width="6" height="6" fill="#4080c0"/></g>"##),
    );
}

#[test]
fn sequential_filters_preserve_both_crops() {
    let source = svg(r##"<defs>
<filter id="a" filterUnits="userSpaceOnUse" x="2" y="2" width="8" height="10"><feOffset/></filter>
<filter id="b" filterUnits="userSpaceOnUse" x="5" y="0" width="8" height="8"><feOffset/></filter>
</defs><rect width="16" height="16" fill="#4080c0" filter="url(#a) url(#b)"/>"##);
    let (_, actual) = converted(&source);
    compare(
        &actual,
        &svg(r##"<rect x="5" y="2" width="5" height="6" fill="#4080c0"/>"##),
    );
}

#[test]
fn sourcealpha_and_real_filter_effects_remain_explicit() {
    for primitive in [
        r#"<feOffset in="SourceAlpha"/>"#,
        r#"<feOffset dx="1"/>"#,
        r#"<feOffset dy="-.25"/>"#,
        r#"<feGaussianBlur stdDeviation="1" result="blur"/><feOffset in="blur"/>"#,
    ] {
        let source = svg(&format!(
            r##"<defs><filter id="f">{primitive}</filter></defs>
<rect x="1" y="1" width="14" height="14" fill="red" filter="url(#f)"/>"##
        ));
        let error =
            try_convert_svg_source_to_chromium_icon(&source, &SvgToChromiumIconOptions::default())
                .unwrap_err();
        assert!(error.contains("滤镜"), "{error}");
    }
}

#[test]
fn identity_filters_follow_use_instance_transforms() {
    let source = svg(
        r##"<defs><filter id="f" filterUnits="userSpaceOnUse" x="1" y="2" width="3" height="4"><feOffset/></filter>
<g id="shape"><rect width="6" height="8" fill="#4080c0" filter="url(#f)"/></g></defs>
<use href="#shape" x="1"/><use href="#shape" x="9" y="3"/>"##,
    );
    let (_, actual) = converted(&source);
    compare(
        &actual,
        &svg(r##"<rect x="2" y="2" width="3" height="4" fill="#4080c0"/>
<rect x="10" y="5" width="3" height="4" fill="#4080c0"/>"##),
    );
}

#[test]
fn object_bounding_box_filter_region_without_primitive_subregion_is_supported() {
    let source = svg(
        r##"<defs><filter id="f" x=".25" y=".25" width=".5" height=".5" primitiveUnits="objectBoundingBox"><feOffset/></filter></defs>
<rect x="2" y="2" width="12" height="12" fill="#4080c0" filter="url(#f)"/>"##,
    );
    let (_, actual) = converted(&source);
    compare(
        &actual,
        &svg(r##"<rect x="5" y="5" width="6" height="6" fill="#4080c0"/>"##),
    );
}

#[test]
fn uncertain_object_bounding_box_primitive_crop_requires_approximation() {
    for definition in [
        r##"<filter id="f" primitiveUnits="objectBoundingBox"><feOffset x=".25" width=".5"/></filter>"##,
        r##"<filter id="base" primitiveUnits="objectBoundingBox"><feOffset y=".25" height=".5"/></filter>
<filter id="f" href="#base"/>"##,
    ] {
        let source = svg(&format!(
            r##"<defs>{definition}</defs>
<style>.filtered {{ filter: url(#f); }}</style>
<rect x="2" y="2" width="12" height="12" fill="red" class="filtered"/>"##
        ));
        let error =
            try_convert_svg_source_to_chromium_icon(&source, &SvgToChromiumIconOptions::default())
                .unwrap_err();
        assert!(error.contains("objectBoundingBox"), "{error}");
    }
}

#[test]
fn unused_uncertain_filter_does_not_block_safe_filter_or_inherited_override() {
    for definition in [
        r##"<filter id="unsafe" primitiveUnits="objectBoundingBox"><feOffset x=".25" width=".5"/></filter>
<filter id="f"><feOffset/></filter>"##,
        r##"<filter id="unsafe" primitiveUnits="objectBoundingBox"><feOffset x="2" y="2" width="8" height="8"/></filter>
<filter id="f" primitiveUnits="userSpaceOnUse" href="#unsafe"/>"##,
    ] {
        let source = svg(&format!(
            r##"<defs>{definition}</defs>
<rect x="2" y="2" width="6" height="6" fill="red" filter="url(#f)"/>"##
        ));
        let (_, actual) = converted(&source);
        compare(
            &actual,
            &svg(r##"<rect x="2" y="2" width="6" height="6" fill="red"/>"##),
        );
    }
}
