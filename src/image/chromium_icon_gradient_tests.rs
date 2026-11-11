use super::{
    tests::{render, svg},
    *,
};

fn template_options() -> SvgToChromiumIconOptions {
    SvgToChromiumIconOptions {
        emit_path_colors: false,
        ..Default::default()
    }
}

fn gradient(kind: &str, attrs: &str, stop_alpha: &str, body: &str) -> String {
    svg(&format!(
        r#"<defs><{kind} id="g" {attrs}><stop stop-color="red" stop-opacity="{stop_alpha}"/><stop offset="1" stop-color="blue" stop-opacity="{stop_alpha}"/></{kind}></defs>{body}"#
    ))
}

#[test]
fn constant_alpha_gradients_remain_vectors_and_inherit_template_color() {
    for (kind, attrs) in [
        (
            "linearGradient",
            r#"gradientTransform="rotate(25)" spreadMethod="reflect""#,
        ),
        (
            "radialGradient",
            r#"fx="35%" fy="40%" spreadMethod="repeat""#,
        ),
    ] {
        for (alpha, expected_alpha) in [("1", 93), ("0.2", 51), ("0", 0)] {
            let source = gradient(
                kind,
                attrs,
                alpha,
                r#"<path d="M2 2H14V14H2Z M5 5H11V11H5Z" fill="url(#g)" fill-rule="evenodd"/>"#,
            );
            let icon =
                try_convert_svg_source_to_chromium_icon(&source, &template_options()).unwrap();
            assert!(!icon.contains("Approximation") && !icon.contains("Sampled"));
            assert!(!icon.contains("PATH_COLOR_ARGB"));
            if alpha == "1" {
                assert!(!icon.contains("PATH_COLOR_ALPHA"));
            }
            let actual =
                render(&reverse::convert_with_color(&icon, None, [20, 180, 60, 93]).unwrap());
            assert_eq!(actual.pixel(24, 24).unwrap().alpha(), expected_alpha);
            assert_eq!(actual.pixel(64, 64).unwrap().alpha(), 0);
            if expected_alpha > 0 {
                assert!(
                    actual.pixel(24, 24).unwrap().green() > actual.pixel(24, 24).unwrap().red()
                );
            }
        }
    }
}

#[test]
fn gradient_stop_alpha_multiplies_fill_stroke_and_group_opacity() {
    let source = gradient(
        "linearGradient",
        r#"gradientUnits="userSpaceOnUse" x2="16""#,
        "0.5",
        r#"<g opacity=".5" transform="translate(1 1) scale(.8)"><rect x="2" y="2" width="12" height="12" fill="url(#g)" fill-opacity=".4"/></g>
<path d="M2 14H14" fill="none" stroke="url(#g)" stroke-opacity=".6" stroke-width="1"/>"#,
    );
    let icon = try_convert_svg_source_to_chromium_icon(&source, &template_options()).unwrap();
    assert!(icon.contains("PATH_COLOR_ALPHA, 26,"));
    assert!(icon.contains("PATH_COLOR_ALPHA, 77,"));
    let actual = render(&reverse::convert(&icon, None).unwrap());
    let expected = render(&source);
    let mean_alpha_error = expected
        .pixels()
        .iter()
        .zip(actual.pixels())
        .map(|(a, b)| f64::from(a.alpha().abs_diff(b.alpha())))
        .sum::<f64>()
        / expected.pixels().len() as f64;
    assert!(mean_alpha_error < 1.0, "{mean_alpha_error}");
}

#[test]
fn transparent_gradient_does_not_constrain_other_layers_clip() {
    let source = gradient(
        "linearGradient",
        "",
        "0",
        r#"<defs><clipPath id="c"><rect width="4" height="4"/></clipPath></defs>
<rect width="16" height="16" fill="red"/>
<rect width="16" height="16" fill="url(#g)" stroke="url(#g)" stroke-width="2" clip-path="url(#c)"/>"#,
    );
    let icon = try_convert_svg_source_to_chromium_icon(&source, &template_options()).unwrap();
    assert!(!icon.contains("NEW_PATH"));
    let actual = render(&reverse::convert(&icon, None).unwrap());
    assert!(actual.pixels().iter().all(|pixel| pixel.alpha() == 255));
}

#[test]
fn color_gradients_variable_alpha_and_patterns_require_explicit_approximation() {
    let colored = gradient(
        "linearGradient",
        "",
        "1",
        r#"<rect width="16" height="16" fill="url(#g)"/>"#,
    );
    let error = try_convert_svg_source_to_chromium_icon_detailed(&colored, &Default::default())
        .unwrap_err();
    assert_eq!(error.kind, IconConversionErrorKind::RequiresApproximation);
    assert!(error.message.contains("彩色渐变"));
    let variable = colored.replace(
        r#"offset="1" stop-color="blue" stop-opacity="1""#,
        r#"offset="1" stop-color="blue" stop-opacity=".3""#,
    );
    let error = try_convert_svg_source_to_chromium_icon_detailed(&variable, &template_options())
        .unwrap_err();
    assert_eq!(error.kind, IconConversionErrorKind::RequiresApproximation);
    assert!(error.message.contains("透明度渐变"));
    let pattern = svg(
        r#"<defs><pattern id="p" width="4" height="4" patternUnits="userSpaceOnUse"><rect width="2" height="2"/></pattern></defs><rect width="16" height="16" fill="url(#p)"/>"#,
    );
    assert!(
        try_convert_svg_source_to_chromium_icon(&pattern, &template_options())
            .unwrap_err()
            .contains("图案")
    );
    let approximate = try_convert_svg_source_to_chromium_icon(
        &variable,
        &SvgToChromiumIconOptions {
            approximate_effects: true,
            ..template_options()
        },
    )
    .unwrap();
    assert!(approximate.contains("Approximation requested"));
    assert!(!approximate.contains("PATH_COLOR_ARGB"));
}

#[test]
fn radial_gradient_with_outside_focal_point_is_not_made_solid() {
    let source = gradient(
        "radialGradient",
        r#"fx="150%" fy="50%""#,
        "1",
        r#"<rect width="16" height="16" fill="url(#g)"/>"#,
    );
    let error =
        try_convert_svg_source_to_chromium_icon_detailed(&source, &template_options()).unwrap_err();
    assert_eq!(error.kind, IconConversionErrorKind::RequiresApproximation);
    assert!(error.message.contains("焦点"));
    for attrs in [
        r#"gradientUnits="userSpaceOnUse" cx="8" cy="8" r="4" fx="11.9996" fy="8""#,
        r#"gradientUnits="userSpaceOnUse" cx="8" cy="8" r="0.0001""#,
    ] {
        let source = gradient(
            "radialGradient",
            attrs,
            "1",
            r#"<rect width="16" height="16" fill="url(#g)"/>"#,
        );
        let error = try_convert_svg_source_to_chromium_icon_detailed(&source, &template_options())
            .unwrap_err();
        assert_eq!(error.kind, IconConversionErrorKind::RequiresApproximation);
    }
}

#[test]
fn template_policy_does_not_remove_luminance_gradient_mask() {
    let source = gradient(
        "linearGradient",
        "",
        "1",
        r#"<mask id="m"><rect width="16" height="16" fill="url(#g)"/></mask><rect width="16" height="16" fill="red" mask="url(#m)"/>"#,
    );
    let error =
        try_convert_svg_source_to_chromium_icon_detailed(&source, &template_options()).unwrap_err();
    assert_eq!(error.kind, IconConversionErrorKind::RequiresApproximation);
    assert!(error.message.contains("渐变"));
}

#[test]
fn identity_filter_reveals_gradient_requirement_and_template_converts() {
    let source = gradient(
        "linearGradient",
        "",
        "1",
        r#"<defs><filter id="f"><feOffset dx="0" dy="0"/></filter></defs><rect x="1" y="1" width="14" height="14" fill="url(#g)" filter="url(#f)"/>"#,
    );
    let error =
        try_convert_svg_source_to_chromium_icon_detailed(&source, &Default::default()).unwrap_err();
    assert_eq!(error.kind, IconConversionErrorKind::RequiresApproximation);
    assert!(error.message.contains("彩色渐变"));
    assert!(!error.message.contains("滤镜"));
    let icon = try_convert_svg_source_to_chromium_icon(&source, &template_options()).unwrap();
    assert!(!icon.contains("Approximation"));
    let actual = render(&reverse::convert(&icon, None).unwrap());
    assert_eq!(actual.pixel(64, 64).unwrap().alpha(), 255);
    assert_eq!(actual.pixel(0, 0).unwrap().alpha(), 0);
    let error =
        try_convert_svg_source_to_chromium_icon_detailed("<svg>", &Default::default()).unwrap_err();
    assert_eq!(error.kind, IconConversionErrorKind::Failed);
}
