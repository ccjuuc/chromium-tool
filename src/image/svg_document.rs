//! Shared SVG parsing for vector and raster outputs.
//!
//! Resolve XML/URL references once, then let usvg resolve CSS, geometry,
//! viewports and inheritance instead of guessing presentation rules.

use std::collections::{HashMap, HashSet};

fn decode_fragment(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(a), Some(b)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            ) {
                decoded.push((a * 16 + b) as u8);
                i += 3;
                continue;
            }
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(decoded).unwrap_or_else(|_| value.to_string())
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Give referenced IDs containing spaces safe aliases for usvg's IRI parser.
/// Work with XML-decoded values and source ranges to preserve entity escaping.
pub(crate) fn prepare_svg(content: &str) -> Result<String, String> {
    let document =
        usvg::roxmltree::Document::parse(content).map_err(|e| format!("SVG XML 解析失败: {e}"))?;
    if !document.root_element().has_tag_name("svg") {
        return Err("输入必须是 SVG 文档".to_string());
    }
    for node in document.descendants().filter(|node| node.is_element()) {
        match node.tag_name().name() {
            "text" | "textPath" | "tspan" => {
                return Err("SVG 含文字；请先将文字转为路径，以避免字体替换或文字丢失".to_string());
            }
            "foreignObject" => {
                return Err("SVG foreignObject 无法可靠转换；请先转为路径或嵌入式图片".to_string());
            }
            "animate" | "animateMotion" | "animateTransform" | "set" => {
                return Err("不支持动态 SVG；请先导出需要的静态帧".to_string());
            }
            "image" | "feImage" | "use" => {
                for attr in node.attributes().filter(|attr| attr.name() == "href") {
                    let href = attr.value().trim();
                    let local = href.starts_with('#');
                    let embedded = node.tag_name().name() != "use" && href.starts_with("data:");
                    if embedded
                        && href[5..]
                            .split([';', ','])
                            .next()
                            .is_some_and(|mime| mime.eq_ignore_ascii_case("image/svg+xml"))
                    {
                        return Err("不支持通过 data URL 嵌入 SVG；请将矢量内容合并到主 SVG，或嵌入 PNG/JPEG 图片".to_string());
                    }
                    if !href.is_empty() && !local && !embedded {
                        return Err("SVG 外部资源引用不受支持；请将资源嵌入 SVG".to_string());
                    }
                }
            }
            "style" if node.text().is_some_and(|text| text.contains("@import")) => {
                return Err("SVG 外部样式表不受支持；请内嵌样式".to_string());
            }
            _ => {}
        }
    }
    let mut ids: HashSet<String> = document
        .descendants()
        .filter_map(|node| node.attribute("id").map(str::to_string))
        .collect();
    let url = regex::Regex::new(r##"url\(\s*['"]?#([^'")]+)['"]?\s*\)"##).unwrap();
    let mut referenced: HashSet<String> = document
        .descendants()
        .flat_map(|node| node.attributes())
        .filter(|attr| attr.name() == "href")
        .filter_map(|attr| attr.value().trim().strip_prefix('#').map(decode_fragment))
        .collect();
    for node in document.descendants() {
        for value in node
            .attributes()
            .map(|attr| attr.value())
            .chain(node.is_text().then(|| node.text().unwrap_or_default()))
        {
            referenced.extend(
                url.captures_iter(value)
                    .map(|caps| decode_fragment(caps[1].trim())),
            );
        }
    }
    let mut aliases = HashMap::new();
    for node in document.descendants() {
        if let Some(id) = node
            .attribute("id")
            .filter(|id| id.contains(char::is_whitespace) && referenced.contains(*id))
        {
            if aliases.contains_key(id) {
                continue;
            }
            let mut index = aliases.len();
            let alias = loop {
                let candidate = format!("chromium_tool_instance_{index}");
                if ids.insert(candidate.clone()) {
                    break candidate;
                }
                index += 1;
            };
            aliases.insert(id.to_string(), alias);
        }
    }
    let resolve = |fragment: &str| {
        let decoded = decode_fragment(fragment);
        aliases.get(&decoded).cloned().unwrap_or(decoded)
    };
    let id_selector = regex::Regex::new(r##"\[\s*id\s*=\s*(?:"([^"]*)"|'([^']*)')\s*\]"##).unwrap();
    let replace_urls = |value: &str| {
        url.replace_all(value, |caps: &regex::Captures<'_>| {
            let id = decode_fragment(caps[1].trim());
            if let Some(alias) = aliases.get(&id) {
                format!("url(#{alias})")
            } else if id != caps[1].trim() {
                format!("url(#{id})")
            } else {
                caps[0].to_string()
            }
        })
        .into_owned()
    };
    let mut edits = Vec::new();
    for node in document.descendants() {
        for attr in node.attributes() {
            let value = attr.value();
            let replacement = if attr.name() == "id" {
                aliases
                    .get(value)
                    .cloned()
                    .unwrap_or_else(|| value.to_string())
            } else if attr.name() == "href" && value.trim().starts_with('#') {
                format!("#{}", resolve(&value.trim()[1..]))
            } else {
                replace_urls(value)
            };
            if replacement != value {
                edits.push((attr.range_value(), xml_escape(&replacement)));
            }
        }
        if node.is_text() && node.parent().is_some_and(|p| p.has_tag_name("style")) {
            let value = node.text().unwrap_or_default();
            let replacement = replace_urls(value);
            let replacement =
                id_selector.replace_all(&replacement, |caps: &regex::Captures<'_>| {
                    let id = caps.get(1).or_else(|| caps.get(2)).unwrap().as_str();
                    aliases
                        .get(id)
                        .map(|alias| format!("[id=\"{alias}\"]"))
                        .unwrap_or_else(|| caps[0].to_string())
                });
            if replacement != value {
                edits.push((node.range(), xml_escape(&replacement)));
            }
        }
    }
    let mut prepared = content.to_string();
    edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    for (range, replacement) in edits {
        prepared.replace_range(range, &replacement);
    }
    Ok(prepared)
}

pub(crate) fn parse_svg(content: &str) -> Result<usvg::Tree, String> {
    let prepared = prepare_svg(content)?;
    let embedded_svg = std::sync::atomic::AtomicBool::new(false);
    let resolver = usvg::ImageHrefResolver::default_data_resolver();
    let mut options = usvg::Options::default();
    // The default resolver can open arbitrary local paths. Submitted documents
    // can use embedded raster images only.
    options.image_href_resolver.resolve_string = Box::new(|_, _| None);
    // SVG can also be embedded with an omitted or text/plain media type.
    // Reject it explicitly because usvg's nested parser bypasses prepare_svg.
    options.image_href_resolver.resolve_data =
        Box::new(|mime, data, options| match resolver(mime, data, options) {
            Some(usvg::ImageKind::SVG(_)) => {
                embedded_svg.store(true, std::sync::atomic::Ordering::Relaxed);
                None
            }
            image => image,
        });
    let result = usvg::Tree::from_str(&prepared, &options);
    if embedded_svg.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(
            "不支持通过 data URL 嵌入 SVG；请将矢量内容合并到主 SVG，或嵌入 PNG/JPEG 图片"
                .to_string(),
        );
    }
    result.map_err(|e| format!("SVG 解析失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_preserve_entities_selectors_and_nested_references() {
        let source = r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="16" height="16">
<style>[id="ray &amp; plus+"] { fill: red; }</style>
<use href="#copy" x="8"/>
<defs>
 <rect id="chromium_tool_instance_0" width="2" height="2" fill="blue"/>
 <g id="ray &amp; plus+"><rect width="2" height="2"/></g>
 <g id="copy"><use xlink:href="#ray%20%26%20plus%2B"/></g>
</defs>
<use href="#ray &amp; plus+" y="8"/>
</svg>"##;
        let tree = parse_svg(source).unwrap();
        let mut image = tiny_skia::Pixmap::new(16, 16).unwrap();
        resvg::render(&tree, tiny_skia::Transform::identity(), &mut image.as_mut());
        for (x, y) in [(9, 1), (1, 9)] {
            let pixel = image.pixel(x, y).unwrap().demultiply();
            assert_eq!(
                (pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()),
                (255, 0, 0, 255)
            );
        }
        assert_eq!(image.pixel(1, 1).unwrap().alpha(), 0);
    }

    #[test]
    fn rejects_external_images_and_unoutlined_text() {
        for content in [
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><image href="C:/private.png" width="16" height="16"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><text x="0" y="12">Hello</text></svg>"#,
        ] {
            assert!(parse_svg(content).is_err());
        }
    }

    #[test]
    fn url_fragments_with_spaces_and_percent_escapes_resolve() {
        let source = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16">
<defs><clipPath id="left half"><rect width="8" height="16"/></clipPath></defs>
<rect width="16" height="16" fill="red" clip-path="url(#left%20half)"/>
</svg>"##;
        let tree = parse_svg(source).unwrap();
        let mut image = tiny_skia::Pixmap::new(16, 16).unwrap();
        resvg::render(&tree, tiny_skia::Transform::identity(), &mut image.as_mut());
        assert_eq!(image.pixel(4, 8).unwrap().alpha(), 255);
        assert_eq!(image.pixel(12, 8).unwrap().alpha(), 0);
    }

    #[test]
    fn rejects_nested_svg_but_keeps_embedded_png_and_jpeg() {
        use base64::Engine;
        let svg = base64::engine::general_purpose::STANDARD.encode(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"><rect width="1" height="1"/></svg>"#
        );
        for mime in ["image/svg+xml", "text/plain", ""] {
            let source = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><image width="16" height="16" href="data:{mime};base64,{svg}"/></svg>"#
            );
            assert!(parse_svg(&source).unwrap_err().contains("嵌入 SVG"));
        }
        for (mime, format) in [
            ("image/png", image::ImageFormat::Png),
            ("image/jpeg", image::ImageFormat::Jpeg),
        ] {
            let pixels = image::DynamicImage::ImageRgb8(image::ImageBuffer::from_pixel(
                2,
                2,
                image::Rgb([255u8, 0, 0]),
            ));
            let mut bytes = std::io::Cursor::new(Vec::new());
            pixels.write_to(&mut bytes, format).unwrap();
            let encoded = base64::engine::general_purpose::STANDARD.encode(bytes.into_inner());
            let source = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><image width="16" height="16" href="data:{mime};base64,{encoded}"/></svg>"#
            );
            let tree = parse_svg(&source).unwrap();
            let mut image = tiny_skia::Pixmap::new(16, 16).unwrap();
            resvg::render(&tree, tiny_skia::Transform::identity(), &mut image.as_mut());
            assert_eq!(image.pixel(8, 8).unwrap().alpha(), 255);
            assert!(image.pixel(8, 8).unwrap().red() > 250);
        }
    }
}
