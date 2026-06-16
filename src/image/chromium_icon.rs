use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use svg::node::element::path::{Command, Data, Position};
use svg::node::element::tag::Type;
use svg::node::element::{Circle, Ellipse, Path as SvgPath, Rectangle};
use svg::node::Value;
use svg::parser::Event;
use svg::Document;

// ===========================================================================
//
// 本文件实现 SVG <-> Chromium `.icon` 文件之间的双向转换。
//
// Chromium `.icon` 文件格式参考：
//   ui/gfx/vector_icon_types.h        // 命令枚举定义
//   ui/gfx/vector_icon_utils.cc       // ParsePathElement / GetCommandArgumentCount
//   ui/gfx/paint_vector_icon.cc       // PathParser / PaintPath 实际绘制
//   components/vector_icons/aggregate_vector_icons.py  // 多分辨率 .icon 聚合
//
// 注意：
//   * Chromium 的默认填充规则是 EvenOdd（与 SVG 默认 NonZero 相反），
//     因此只有当 SVG 显式指定 fill-rule="nonzero"（或缺省）时才需要写出
//     `FILL_RULE_NONZERO`。Chromium 中并不存在 `FILL_RULE_EVENODD` 命令。
//   * Chromium 平滑曲线命令叫 `CUBIC_TO_SHORTHAND` / `QUADRATIC_TO_SHORTHAND`，
//     且 **没有** `R_CUBIC_TO_SHORTHAND`（只有 `R_QUADRATIC_TO_SHORTHAND`）。
//     因此相对的 `s` 命令必须就地展开为绝对坐标的 `CUBIC_TO_SHORTHAND`。
//   * SVG 路径数据中的命令字母可携带多组参数，第二组以后须按隐式命令规则
//     展开（M 后续是 L/l，C 后续是 C/c 等）。
//
// ===========================================================================

/// 将一个浮点数格式化为 `.icon` 文件中使用的紧凑数字串。
///
/// Chromium 实际 `.icon` 文件中的写法是 `1.5`、`-0.97`、`24` 等，**不带 `f` 后缀**。
/// 例如 `components/vector_icons/account_circle.icon`：
///     `MOVE_TO, 5.85, 17.1,`
///     `R_QUADRATIC_TO, 1.27, -0.97, 2.85, -1.54,`
///
/// 虽然 `vector_icon_utils.cc::ParsePathElement` 也能解析末尾带 `f` 的数字，
/// 但与既有 .icon 文件保持一致，这里不输出 `f` 后缀。
/// 坐标"近整数吸附"阈值：与最近整数的距离 ≤ 此值时直接吸附到整数。
///
/// 目的：本应落在整数像素网格上的边缘，常因浮点运算（旋转/缩放/平移）或导出工具
/// 的微小误差偏移一点点（如 `11.9998`、`2.0001`），在渲染时跨越像素边界而发虚。
/// 吸附回整数即可规避。阈值 0.05 远小于 1px 描边常用的 `.5` 居中（距整数 0.5），
/// 因此**不会**破坏 `1.5 / 2.5` 这类有意的半像素锐利描边。
const COORD_INTEGER_SNAP_EPS: f32 = 0.05;

fn format_number(num: f32) -> String {
    if num.is_nan() {
        return "0".to_string();
    }

    // 接近整数（含精确整数）的值统一吸附到整数：既避免 `1.0` 这类冗余小数，
    // 也消除浮点噪声/导出误差导致的非整数发虚。
    let nearest = num.round();
    if (num - nearest).abs() <= COORD_INTEGER_SNAP_EPS && nearest.abs() < 1.0e9 {
        let n = nearest as i64;
        // 避免 `-0`。
        return format!("{}", if n == 0 { 0 } else { n });
    }

    // 其余值四舍五入到 2 位小数（此前为截断：会让 6.999 落成 6.99 反而更偏），
    // 再去掉末尾多余的 0 与可能残留的小数点。
    let rounded = (num * 100.0).round() / 100.0;
    let mut s = format!("{:.2}", rounded);
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    // 处理 `-0` 情况
    if s == "-0" {
        s = "0".to_string();
    }
    s
}

/// 把 SVG 颜色字符串（`#RGB` / `#RGBA` / `#RRGGBB` / `#RRGGBBAA` / 关键字）
/// 转换为 `PATH_COLOR_ARGB` 命令使用的 `0xAA, 0xRR, 0xGG, 0xBB` 形式。
///
/// 不可识别或 `none` 等非颜色值返回空串，调用方据此跳过 `PATH_COLOR_ARGB` 输出。
fn color_to_argb(color: &str) -> String {
    let color = color.trim();
    if color.is_empty() || color.eq_ignore_ascii_case("none") {
        return String::new();
    }

    if let Some(argb) = css_rgb_or_rgba_to_argb_components(color) {
        return argb;
    }

    // 处理 `#xxx` 形式
    if let Some(hex) = color.strip_prefix('#') {
        let argb = match hex.len() {
            // #RGB -> 扩展为 #RRGGBB
            3 => {
                let r = expand_nibble(&hex[0..1]);
                let g = expand_nibble(&hex[1..2]);
                let b = expand_nibble(&hex[2..3]);
                Some(format!("0xFF, 0x{}, 0x{}, 0x{}", r, g, b))
            }
            // #RGBA -> 扩展为 #RRGGBBAA
            4 => {
                let r = expand_nibble(&hex[0..1]);
                let g = expand_nibble(&hex[1..2]);
                let b = expand_nibble(&hex[2..3]);
                let a = expand_nibble(&hex[3..4]);
                Some(format!("0x{}, 0x{}, 0x{}, 0x{}", a, r, g, b))
            }
            6 => {
                let r = &hex[0..2];
                let g = &hex[2..4];
                let b = &hex[4..6];
                Some(format!("0xFF, 0x{}, 0x{}, 0x{}", r, g, b))
            }
            8 => {
                // SVG/CSS 的 #RRGGBBAA：alpha 在末尾；Chromium PATH_COLOR_ARGB 的 alpha 在第一位。
                let r = &hex[0..2];
                let g = &hex[2..4];
                let b = &hex[4..6];
                let a = &hex[6..8];
                Some(format!("0x{}, 0x{}, 0x{}, 0x{}", a, r, g, b))
            }
            _ => None,
        };
        if let Some(s) = argb {
            return s;
        }
    }

    // CSS 颜色关键字（按 CSS Color Module Level 3，不区分大小写）。
    match color.to_ascii_lowercase().as_str() {
        "transparent" => "0x00, 0x00, 0x00, 0x00".to_string(),
        "black" => "0xFF, 0x00, 0x00, 0x00".to_string(),
        "white" => "0xFF, 0xFF, 0xFF, 0xFF".to_string(),
        "red" => "0xFF, 0xFF, 0x00, 0x00".to_string(),
        // CSS 中 `green` 是 #008000（深绿），`lime` 才是 #00FF00。
        // 旧实现把 `green` 写成 `0x00, 0xFF, 0x00`，这里予以纠正。
        "green" => "0xFF, 0x00, 0x80, 0x00".to_string(),
        "lime" => "0xFF, 0x00, 0xFF, 0x00".to_string(),
        "blue" => "0xFF, 0x00, 0x00, 0xFF".to_string(),
        "yellow" => "0xFF, 0xFF, 0xFF, 0x00".to_string(),
        "cyan" | "aqua" => "0xFF, 0x00, 0xFF, 0xFF".to_string(),
        "magenta" | "fuchsia" => "0xFF, 0xFF, 0x00, 0xFF".to_string(),
        "gray" | "grey" => "0xFF, 0x80, 0x80, 0x80".to_string(),
        "silver" => "0xFF, 0xC0, 0xC0, 0xC0".to_string(),
        "maroon" => "0xFF, 0x80, 0x00, 0x00".to_string(),
        "olive" => "0xFF, 0x80, 0x80, 0x00".to_string(),
        "purple" => "0xFF, 0x80, 0x00, 0x80".to_string(),
        "teal" => "0xFF, 0x00, 0x80, 0x80".to_string(),
        "navy" => "0xFF, 0x00, 0x00, 0x80".to_string(),
        _ => String::new(),
    }
}

fn expand_nibble(c: &str) -> String {
    format!("{0}{0}", c)
}

/// 解析 `rgb(r,g,b)` / `rgba(r,g,b,a)`（逗号或空白分隔），`a` 可为 0–255 或 0.0–1.0。
fn css_rgb_or_rgba_to_argb_components(color: &str) -> Option<String> {
    let c = color.trim();
    let lower = c.to_ascii_lowercase();
    let inner = lower
        .strip_prefix("rgba(")
        .or_else(|| lower.strip_prefix("rgb("))?
        .strip_suffix(')')?;

    let parts: Vec<&str> = inner
        .split(|ch: char| ch == ',' || ch.is_ascii_whitespace())
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() < 3 {
        return None;
    }
    let r = parts[0].parse::<u8>().ok()?;
    let g = parts[1].parse::<u8>().ok()?;
    let b = parts[2].parse::<u8>().ok()?;
    let has_alpha = lower.starts_with("rgba(");
    let a = if has_alpha && parts.len() >= 4 {
        let ap = parts[3];
        if let Ok(ai) = ap.parse::<u8>() {
            ai
        } else if let Ok(af) = ap.parse::<f64>() {
            (af.clamp(0.0, 1.0) * 255.0).round() as u8
        } else {
            return None;
        }
    } else {
        255
    };
    Some(format!(
        "0x{:02X}, 0x{:02X}, 0x{:02X}, 0x{:02X}",
        a, r, g, b
    ))
}

/// 把 `style="fill: #abc; stroke-width: 1"` 这类 CSS 声明字符串解析为 map。
/// key 统一小写，value 保留原始大小写以便颜色 hex 不变。
fn parse_inline_style_decls(style: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for decl in style.split(';') {
        let decl = decl.trim();
        if decl.is_empty() {
            continue;
        }
        if let Some((k, v)) = decl.split_once(':') {
            out.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    out
}

fn strip_css_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        match rest[start + 2..].find("*/") {
            Some(end) => rest = &rest[start + 2 + end + 2..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// 一个迷你 CSS 解析器，仅识别形如 `selector1, selector2 { prop: value; ... }` 的规则。
///
/// 不支持 `@media`、`:hover` 等高级语法（也基本不会在静态 SVG 资源里出现）。
/// 对每个选择器返回其声明集合；同选择器多次出现时按出现顺序合并（后者覆盖前者）。
fn parse_svg_css(text: &str) -> std::collections::HashMap<String, std::collections::HashMap<String, String>> {
    let cleaned = strip_css_comments(text);
    let mut sheet: std::collections::HashMap<String, std::collections::HashMap<String, String>> =
        std::collections::HashMap::new();
    let mut cursor = cleaned.as_str();
    loop {
        let lb = match cursor.find('{') {
            Some(p) => p,
            None => break,
        };
        let selector_part = cursor[..lb].trim();
        let after_lb = &cursor[lb + 1..];
        let rb = match after_lb.find('}') {
            Some(p) => p,
            None => break,
        };
        let body = &after_lb[..rb];
        let decls = parse_inline_style_decls(body);
        if !selector_part.is_empty() && !decls.is_empty() {
            for sel in selector_part.split(',') {
                let sel = sel.trim();
                if sel.is_empty() {
                    continue;
                }
                let entry = sheet.entry(sel.to_string()).or_default();
                for (k, v) in &decls {
                    entry.insert(k.clone(), v.clone());
                }
            }
        }
        cursor = &after_lb[rb + 1..];
    }
    sheet
}

/// 扫描事件流，把所有 `<style>...</style>` 中的 CSS 文本拼接后解析为 stylesheet。
///
/// 之所以需要这个：很多在线 SVG（svgrepo 等）会用 CSS 类来设置 `fill`，
/// 而我们正向转换 (`handle_svg_*`) 只看 inline `fill=` 属性，导致颜色全部丢失，
/// 反向预览时所有路径退化成同一个 fallback 颜色，整张图看起来变成"白板"。
fn collect_svg_stylesheet(
    events: &[Event<'_>],
) -> std::collections::HashMap<String, std::collections::HashMap<String, String>> {
    let mut depth: usize = 0;
    let mut buf = String::new();
    let mut sheet: std::collections::HashMap<String, std::collections::HashMap<String, String>> =
        std::collections::HashMap::new();
    for ev in events {
        match ev {
            Event::Tag("style", t, _) => match t {
                Type::Start => depth += 1,
                Type::End => {
                    if depth > 0 {
                        depth -= 1;
                        if depth == 0 {
                            for (sel, decls) in parse_svg_css(&buf) {
                                let entry = sheet.entry(sel).or_default();
                                for (k, v) in decls {
                                    entry.insert(k, v);
                                }
                            }
                            buf.clear();
                        }
                    }
                }
                Type::Empty => {}
            },
            Event::Text(text) if depth > 0 => {
                buf.push_str(text);
            }
            _ => {}
        }
    }
    sheet
}

/// 按 SVG/CSS 优先级解析出元素最终生效的 `fill` / `fill-rule`：
///
/// 优先级（高 → 低）：inline `style="fill:..."` > CSS class > CSS tag selector > 表现属性 `fill="..."`
///
/// 返回新的 attributes（`fill` / `fill-rule` 已被覆盖），其它键原样保留。
fn resolve_svg_styles(
    stylesheet: &std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    attributes: &std::collections::HashMap<String, Value>,
    tag: &str,
) -> std::collections::HashMap<String, Value> {
    let mut effective: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    if let Some(decls) = stylesheet.get(tag) {
        for (k, v) in decls {
            effective.insert(k.clone(), v.clone());
        }
    }
    if let Some(class_attr) = attributes.get("class") {
        for class in class_attr.to_string().split_whitespace() {
            let key = format!(".{}", class);
            if let Some(decls) = stylesheet.get(&key) {
                for (k, v) in decls {
                    effective.insert(k.clone(), v.clone());
                }
            }
        }
    }
    if let Some(style_attr) = attributes.get("style") {
        for (k, v) in parse_inline_style_decls(&style_attr.to_string()) {
            effective.insert(k, v);
        }
    }

    let mut out = attributes.clone();
    for key in ["fill", "fill-rule"] {
        if let Some(v) = effective.get(key) {
            out.insert(key.to_string(), Value::from(v.as_str()));
        }
    }
    out
}

/// `clipPath` / `defs` / `symbol` 等子树不参与可见绘制（其中的 rect 常为剪切白底）。
/// 必须与标签名大小写无关（SVG DOM 中为 `clipPath`）。
fn is_svg_non_paint_subtree(tag: &str) -> bool {
    tag.eq_ignore_ascii_case("defs")
        || tag.eq_ignore_ascii_case("clipPath")
        || tag.eq_ignore_ascii_case("mask")
        || tag.eq_ignore_ascii_case("marker")
        || tag.eq_ignore_ascii_case("pattern")
        || tag.eq_ignore_ascii_case("linearGradient")
        || tag.eq_ignore_ascii_case("radialGradient")
        || tag.eq_ignore_ascii_case("filter")
        || tag.eq_ignore_ascii_case("symbol")
}

/// `fill` 视为「不向 Chromium 写入可见填充」：缺失 / `none` / 透明 / `currentColor`。
fn svg_paint_fill_is_none_like(v: Option<&Value>) -> bool {
    v.map(|x| {
        let t = x.to_string().trim().to_ascii_lowercase();
        t.is_empty()
            || t == "none"
            || t == "transparent"
            || t == "currentcolor"
    })
    .unwrap_or(true)
}

fn svg_paint_stroke_is_usable(v: Option<&Value>) -> bool {
    v.map(|x| {
        let t = x.to_string().trim().to_ascii_lowercase();
        !t.is_empty() && t != "none" && t != "transparent" && t != "currentcolor"
    })
    .unwrap_or(false)
}

/// `fill="url(#…)"` / `stroke="url(#…)"` 等 SVG paint server，Chromium `.icon` 无法表达。
fn svg_paint_value_is_url_reference(v: Option<&Value>) -> bool {
    v.map(|x| {
        let t = x.to_string().trim().to_ascii_lowercase();
        t.starts_with("url(")
    })
    .unwrap_or(false)
}

fn svg_attr_href_value(attributes: &std::collections::HashMap<String, Value>) -> Option<String> {
    attributes.iter().find_map(|(k, v)| {
        if k.eq_ignore_ascii_case("href") || k.ends_with(":href") {
            Some(v.to_string())
        } else {
            None
        }
    })
}

fn svg_href_points_to_raster_image(href: &str) -> bool {
    let s = href.trim().to_ascii_lowercase();
    s.starts_with("data:image/")
        || s.starts_with("http://")
        || s.starts_with("https://")
        || s.starts_with("file:")
        || s.ends_with(".png")
        || s.ends_with(".jpg")
        || s.ends_with(".jpeg")
        || s.ends_with(".webp")
        || s.ends_with(".gif")
        || s.ends_with(".bmp")
}

/// 转换前校验：拒绝位图头像/照片、pattern 平铺、渐变等无法矢量化的 SVG。
fn validate_svg_vector_convertible(events: &[Event]) -> Result<(), String> {
    let is_open_tag = |t: &Type| matches!(t, Type::Start | Type::Empty);
    let mut has_raster_image = false;
    let mut has_pattern = false;
    let mut has_gradient = false;
    let mut visible_paint_server_fill = false;

    let mut non_paint_depth: usize = 0;
    for event in events {
        match event {
            Event::Tag(name, t, _) if is_svg_non_paint_subtree(name) => {
                if matches!(t, Type::Start | Type::Empty) {
                    if name.eq_ignore_ascii_case("pattern") {
                        has_pattern = true;
                    }
                    if name.eq_ignore_ascii_case("linearGradient")
                        || name.eq_ignore_ascii_case("radialGradient")
                    {
                        has_gradient = true;
                    }
                    non_paint_depth += 1;
                } else if matches!(t, Type::End) {
                    non_paint_depth = non_paint_depth.saturating_sub(1);
                }
            }
            Event::Tag(name, t, attrs) if is_open_tag(t) => {
                if name.eq_ignore_ascii_case("image") {
                    if svg_attr_href_value(attrs)
                        .is_some_and(|h| svg_href_points_to_raster_image(&h))
                    {
                        has_raster_image = true;
                    }
                }
                if non_paint_depth == 0
                    && matches!(name.as_ref(), "path" | "rect" | "circle" | "ellipse")
                {
                    if svg_paint_value_is_url_reference(attrs.get("fill"))
                        || svg_paint_value_is_url_reference(attrs.get("stroke"))
                    {
                        visible_paint_server_fill = true;
                    }
                }
            }
            _ => {}
        }
    }

    if has_raster_image {
        return Err(
            "SVG 含有嵌入式位图（<image> / PNG·JPEG 等）。Chromium .icon 仅支持矢量路径，\
             头像/照片类资源无法直接转换；请使用纯矢量 SVG，或保留为 PNG/ICO 等位图格式。"
                .to_string(),
        );
    }
    if has_pattern && visible_paint_server_fill {
        return Err(
            "SVG 使用 pattern 平铺填充（如 Figma 导出的头像底图）。Chromium .icon 不支持 pattern，\
             无法保留位图内容；请改用纯色/描边的矢量图形。"
                .to_string(),
        );
    }
    if has_gradient && visible_paint_server_fill {
        return Err(
            "SVG 使用渐变填充（linearGradient/radialGradient）。Chromium .icon 不支持渐变，\
             请改为纯色或描边矢量。"
                .to_string(),
        );
    }
    Ok(())
}

fn svg_path_data_has_close_command(d: Option<&Value>) -> bool {
    let Some(v) = d else {
        return false;
    };
    match Data::parse(&v.to_string()) {
        Ok(data) => data.iter().any(|c| matches!(c, Command::Close)),
        Err(_) => false,
    }
}

/// 仅支持 `M/m` + `L/l` + `H/h` + `V/v` 组成的开放子路径；遇 `Z`、曲线、弧等返回 `None`。
fn collect_open_polyline_contours_move_line_only(data: &Data) -> Option<Vec<Vec<(f32, f32)>>> {
    let mut contours: Vec<Vec<(f32, f32)>> = Vec::new();
    let mut cur: Vec<(f32, f32)> = Vec::new();
    let mut pen = PenState::default();

    for command in data.iter() {
        match command {
            Command::Close => return None,
            Command::Move(position, params) => {
                if !cur.is_empty() {
                    contours.push(std::mem::take(&mut cur));
                }
                let mut idx = 0usize;
                let mut first = true;
                while idx + 1 < params.len() {
                    let a = params[idx];
                    let b = params[idx + 1];
                    idx += 2;
                    match position {
                        Position::Absolute => {
                            if first {
                                pen.move_abs(a, b);
                            } else {
                                pen.line_abs(a, b);
                            }
                        }
                        Position::Relative => {
                            if first {
                                pen.move_rel(a, b);
                            } else {
                                pen.line_rel(a, b);
                            }
                        }
                    }
                    cur.push((pen.cur_x, pen.cur_y));
                    first = false;
                }
            }
            Command::Line(position, params) => {
                let mut idx = 0usize;
                while idx + 1 < params.len() {
                    let a = params[idx];
                    let b = params[idx + 1];
                    idx += 2;
                    match position {
                        Position::Absolute => pen.line_abs(a, b),
                        Position::Relative => pen.line_rel(a, b),
                    }
                    cur.push((pen.cur_x, pen.cur_y));
                }
            }
            Command::HorizontalLine(position, params) => {
                for &x in params.iter() {
                    match position {
                        Position::Absolute => pen.line_abs(x, pen.cur_y),
                        Position::Relative => pen.line_rel(x, 0.0),
                    }
                    cur.push((pen.cur_x, pen.cur_y));
                }
            }
            Command::VerticalLine(position, params) => {
                for &y in params.iter() {
                    match position {
                        Position::Absolute => pen.line_abs(pen.cur_x, y),
                        Position::Relative => pen.line_rel(0.0, y),
                    }
                    cur.push((pen.cur_x, pen.cur_y));
                }
            }
            _ => return None,
        }
    }

    if !cur.is_empty() {
        contours.push(cur);
    }

    let usable: Vec<Vec<(f32, f32)>> = contours
        .into_iter()
        .filter(|c| c.len() >= 2)
        .collect();
    if usable.is_empty() {
        return None;
    }
    Some(usable)
}

/// `fill="none"` 且仅直线、无 `Z` 的开放 `<path>`，用 Chromium 原生 `STROKE`（加 `CLOSE`）表达；
/// 仅含单条连通折线时不返回 `None`（多段 `M…M…` 开放折线暂不自动合并，交由通用路径分支）。
///
/// Chromium `PaintVectorIcon`：`NEW_PATH` 后 `PaintFlags` 复位为填充；需显式写入 `STROKE, width`
/// 才能像 Material 矢量图标那样描边，填充四边形在这条管线上并不可靠。
fn try_emit_open_stroked_polyline_as_chromium_stroke_commands(
    attributes: &std::collections::HashMap<String, Value>,
    write_new_path: bool,
    emit_path_colors: bool,
) -> Option<String> {
    let d = attributes.get("d")?;
    let data = Data::parse(&d.to_string()).ok()?;
    let contours = collect_open_polyline_contours_move_line_only(&data)?;
    if contours.len() != 1 {
        return None;
    }
    let verts = contours.first()?;
    if verts.len() < 2 {
        return None;
    }

    let mut output = String::new();
    if write_new_path {
        output.push_str("NEW_PATH,\r\n");
    }
    if emit_path_colors {
        if let Some(st) = attributes.get("stroke") {
            let color = color_to_argb(&st.to_string());
            if !color.is_empty() {
                output.push_str(&format!("PATH_COLOR_ARGB, {},\r\n", color));
            }
        }
    }
    // 与通用 `handle_svg_path` 一致：SVG 路径默认 nonzero。
    output.push_str("FILL_RULE_NONZERO,\r\n");
    let sw = parse_attr_f32(attributes, "stroke-width", 1.0);
    output.push_str(&format!("STROKE, {},\r\n", format_number(sw)));
    let (x0, y0) = verts[0];
    output.push_str(&format!(
        "MOVE_TO, {}, {},\r\n",
        format_number(x0),
        format_number(y0)
    ));
    for (x, y) in verts.iter().skip(1) {
        output.push_str(&format!(
            "LINE_TO, {}, {},\r\n",
            format_number(*x),
            format_number(*y)
        ));
    }
    output.push_str("CLOSE,\r\n");
    Some(output)
}

/// Chromium `.icon` 主要靠 `PATH_COLOR_ARGB` 表达可见色；若没有可写的填充色，
/// 反向预览会把路径当成模板白填充，叠在白衬底上就形成白屏。
///
/// `fill` 缺失或为 `none` / `transparent` / `currentColor` 时，依次尝试：`stroke`
/// （若为非 none 类实色）、根 `<svg fill>`、`#000`。
///
/// 仅用于 `<path>`：**不要**对仅 `stroke` 的 `<circle>` 等套用，否则会变成实心形造成黑块。
fn ensure_fill_for_chromium_vector_paint(
    attrs: &mut std::collections::HashMap<String, Value>,
    svg_root_fill: Option<&str>,
) {
    if !svg_paint_fill_is_none_like(attrs.get("fill")) {
        return;
    }

    if let Some(st) = attrs.get("stroke").cloned() {
        let ts = st.to_string().trim().to_ascii_lowercase();
        if !ts.is_empty() && ts != "none" && ts != "transparent" && ts != "currentcolor" {
            attrs.insert("fill".to_string(), st);
            return;
        }
    }

    if let Some(inh) = svg_root_fill {
        let inh_trim = inh.trim();
        let t = inh_trim.to_ascii_lowercase();
        if !inh_trim.is_empty() && t != "none" && t != "transparent" && t != "currentcolor" {
            attrs.insert("fill".to_string(), Value::from(inh_trim));
            return;
        }
    }

    attrs.insert("fill".to_string(), Value::from("#000000"));
}

/// 解析一个浮点数，宽容地接受 `12px` / `12pt` 这类带单位的写法（仅取数字部分）。
fn parse_dim(value: &Value) -> Option<f64> {
    let s = value.to_string();
    let trimmed = s.trim();
    let end = trimmed
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e' || c == 'E'))
        .unwrap_or(trimmed.len());
    trimmed[..end].parse::<f64>().ok()
}

fn parse_attr_f32(attrs: &std::collections::HashMap<String, Value>, key: &str, default: f32) -> f32 {
    attrs
        .get(key)
        .and_then(|v| parse_dim(v))
        .map(|d| d as f32)
        .unwrap_or(default)
}

/// 从 `viewBox="x y w h"` 中提取宽度。viewBox 既允许空格也允许逗号作为分隔符
/// （SVG 规范的 list-of-numbers 定义）。
fn parse_view_box_width(view_box: &str) -> Option<f64> {
    let parts: Vec<&str> = view_box
        .split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter(|s| !s.is_empty())
        .collect();
    parts.get(2).and_then(|s| s.parse::<f64>().ok())
}

/// SVG 仿射矩阵，按 `[a, b, c, d, e, f]` 存储，对应：
/// `x' = a*x + c*y + e`，`y' = b*x + d*y + f`。
type Affine = [f32; 6];

const AFFINE_IDENTITY: Affine = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// 计算 `m1 * m2`（先应用 `m2`、再应用 `m1`）。
fn affine_mul(m1: &Affine, m2: &Affine) -> Affine {
    [
        m1[0] * m2[0] + m1[2] * m2[1],
        m1[1] * m2[0] + m1[3] * m2[1],
        m1[0] * m2[2] + m1[2] * m2[3],
        m1[1] * m2[2] + m1[3] * m2[3],
        m1[0] * m2[4] + m1[2] * m2[5] + m1[4],
        m1[1] * m2[4] + m1[3] * m2[5] + m1[5],
    ]
}

fn affine_apply(m: &Affine, x: f32, y: f32) -> (f32, f32) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

/// 解析 SVG `transform` 属性为单个仿射矩阵；不支持的内容按恒等忽略。
///
/// 支持 `matrix / translate / scale / rotate / skewX / skewY`，可链式书写，
/// 参数以空白或逗号分隔（与 SVG list-of-numbers 一致）。
fn parse_svg_transform(s: &str) -> Option<Affine> {
    let mut acc = AFFINE_IDENTITY;
    let mut matched_any = false;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // 找到下一个函数名（字母）。
        while i < bytes.len() && !bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        let name_start = i;
        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        if name_start == i {
            break;
        }
        let name = &s[name_start..i];
        // 跳到 '('。
        while i < bytes.len() && bytes[i] != b'(' {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        i += 1; // 跳过 '('
        let args_start = i;
        while i < bytes.len() && bytes[i] != b')' {
            i += 1;
        }
        let args_str = &s[args_start..i.min(bytes.len())];
        if i < bytes.len() {
            i += 1; // 跳过 ')'
        }

        let nums: Vec<f32> = args_str
            .split(|c: char| c == ',' || c.is_ascii_whitespace())
            .filter(|t| !t.is_empty())
            .filter_map(|t| t.parse::<f32>().ok())
            .collect();

        let m = match name {
            "matrix" if nums.len() == 6 => {
                [nums[0], nums[1], nums[2], nums[3], nums[4], nums[5]]
            }
            "translate" if !nums.is_empty() => {
                let tx = nums[0];
                let ty = nums.get(1).copied().unwrap_or(0.0);
                [1.0, 0.0, 0.0, 1.0, tx, ty]
            }
            "scale" if !nums.is_empty() => {
                let sx = nums[0];
                let sy = nums.get(1).copied().unwrap_or(sx);
                [sx, 0.0, 0.0, sy, 0.0, 0.0]
            }
            "rotate" if !nums.is_empty() => {
                let rad = nums[0].to_radians();
                let (sin, cos) = rad.sin_cos();
                let rot = [cos, sin, -sin, cos, 0.0, 0.0];
                if nums.len() >= 3 {
                    let (cx, cy) = (nums[1], nums[2]);
                    let t1 = [1.0, 0.0, 0.0, 1.0, cx, cy];
                    let t2 = [1.0, 0.0, 0.0, 1.0, -cx, -cy];
                    affine_mul(&affine_mul(&t1, &rot), &t2)
                } else {
                    rot
                }
            }
            "skewX" if !nums.is_empty() => {
                [1.0, 0.0, nums[0].to_radians().tan(), 1.0, 0.0, 0.0]
            }
            "skewY" if !nums.is_empty() => {
                [1.0, nums[0].to_radians().tan(), 0.0, 1.0, 0.0, 0.0]
            }
            _ => continue,
        };
        acc = affine_mul(&acc, &m);
        matched_any = true;
    }
    if matched_any {
        Some(acc)
    } else {
        None
    }
}

/// 解析 `url(#id)` / `url("#id")` 形式的 SVG 引用，返回 `#` 后的 id。
fn parse_svg_url_ref(raw: &str) -> Option<String> {
    let s = raw.trim();
    if !s.starts_with("url(") || !s.ends_with(')') {
        return None;
    }
    let inner = s[4..s.len() - 1].trim();
    let inner = inner.strip_prefix('#').unwrap_or(inner);
    let inner = inner.trim_matches('"').trim_matches('\'').trim();
    if inner.is_empty() {
        None
    } else {
        Some(inner.to_string())
    }
}

/// `<mask>` 内用于裁剪的矩形几何（Figma 等导出「内描边」时的典型结构）。
#[derive(Clone, Copy, Debug, PartialEq)]
struct SvgMaskRect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
}

fn parse_svg_rect_geometry(attributes: &std::collections::HashMap<String, Value>) -> SvgMaskRect {
    let x = parse_attr_f32(attributes, "x", 0.0);
    let y = parse_attr_f32(attributes, "y", 0.0);
    let w = parse_attr_f32(attributes, "width", 0.0);
    let h = parse_attr_f32(attributes, "height", 0.0);
    let rx_attr = attributes.get("rx").and_then(parse_dim).map(|d| d as f32);
    let ry_attr = attributes.get("ry").and_then(parse_dim).map(|d| d as f32);
    let r = match (rx_attr, ry_attr) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => 0.0,
    };
    SvgMaskRect { x, y, w, h, r }
}

fn svg_mask_rects_approx_equal(a: SvgMaskRect, b: SvgMaskRect, eps: f32) -> bool {
    (a.x - b.x).abs() <= eps
        && (a.y - b.y).abs() <= eps
        && (a.w - b.w).abs() <= eps
        && (a.h - b.h).abs() <= eps
        && (a.r - b.r).abs() <= eps
}

/// 从 SVG 事件流收集 `<mask id="…">` 内 `<rect>` 几何，供识别「内描边」mask 模式。
fn collect_svg_mask_rects(events: &[Event]) -> std::collections::HashMap<String, Vec<SvgMaskRect>> {
    let mut out: std::collections::HashMap<String, Vec<SvgMaskRect>> =
        std::collections::HashMap::new();
    let mut mask_id_stack: Vec<String> = Vec::new();
    let mut mask_depth: usize = 0;

    for event in events {
        match event {
            Event::Tag(name, t, attrs) if name.eq_ignore_ascii_case("mask") => match t {
                Type::Start | Type::Empty => {
                    let id = attrs
                        .get("id")
                        .map(|v| v.to_string())
                        .unwrap_or_default();
                    mask_id_stack.push(id);
                    mask_depth += 1;
                }
                Type::End => {
                    mask_id_stack.pop();
                    mask_depth = mask_depth.saturating_sub(1);
                }
            },
            Event::Tag("rect", t, attrs)
                if matches!(t, Type::Start | Type::Empty) && mask_depth > 0 =>
            {
                if let Some(id) = mask_id_stack.last() {
                    if !id.is_empty() {
                        out.entry(id.clone())
                            .or_default()
                            .push(parse_svg_rect_geometry(attrs));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Figma「内描边」mask 在 Chromium / Skia 中的等价参数。
///
/// SVG 用 mask 把 `stroke-width` 裁成仅内侧一半；Chromium `.icon` 无 mask，改用
/// `STROKE`（Skia 描边**居中**于路径）。为与 SVG 视觉一致且落在像素网格上清晰
/// （半像素路径 + 整数线宽 → 锐利、不偏淡）：
///
/// - `stroke-width` 减半（例：2 → `STROKE, 1`）
/// - 路径相对 SVG 矩形各边内缩 `stroke-width/4`（例：`(3,3,10,10)` →
///   `(3.5,3.5,9,9)`，与 `browser_window_maximum.icon` 一致）
#[derive(Clone, Copy, Debug, PartialEq)]
struct InsideMaskStrokeAdjust {
    stroke_width: f32,
    /// 每边内缩量 = 原 `stroke-width / 4`，使居中描边对齐半像素网格。
    inset: f32,
}

fn inside_mask_stroke_adjust_for_rect(
    attributes: &std::collections::HashMap<String, Value>,
    masks: &std::collections::HashMap<String, Vec<SvgMaskRect>>,
) -> Option<InsideMaskStrokeAdjust> {
    let mask_ref = attributes.get("mask")?;
    let mask_id = parse_svg_url_ref(&mask_ref.to_string())?;
    let mask_rects = masks.get(&mask_id)?;
    let geom = parse_svg_rect_geometry(attributes);
    let is_inside = mask_rects
        .iter()
        .copied()
        .any(|m| svg_mask_rects_approx_equal(m, geom, 0.02));
    if !is_inside {
        return None;
    }
    let sw = parse_attr_f32(attributes, "stroke-width", 1.0);
    Some(InsideMaskStrokeAdjust {
        stroke_width: sw / 2.0,
        inset: sw / 4.0,
    })
}

/// 把 SVG `<rect>` 转换为 Chromium 命令。
///
/// Chromium 的 `ROUND_RECT` 接受 `(x, y, w, h, r)` —— **只有一个圆角半径**。
/// 因此当 `rx != ry` 时无法用单条命令精确表示，这里取 `min(rx, ry)` 折中。
/// 当 `rx == 0 && ry == 0` 时退化为普通矩形（仍可用 `ROUND_RECT` 半径 0 表示）。
///
/// `transform`（如 `rotate(45 cx cy)`）无法用轴对齐的 `ROUND_RECT` 表达，
/// 否则旋转的矩形会退化成水平/竖直条（典型表现：旋转矩形拼出的「X」关闭图标
/// 变成一条横杠）。此时改为对 4 个角点应用变换后输出为四边形路径。
///
/// 调用方负责在调用前决定是否需要 `NEW_PATH`。
fn handle_svg_rect(
    _tag_type: &Type,
    attributes: &std::collections::HashMap<String, Value>,
    write_new_path: bool,
    emit_path_colors: bool,
    masks: &std::collections::HashMap<String, Vec<SvgMaskRect>>,
) -> String {
    let mut output = String::new();

    let geom = parse_svg_rect_geometry(attributes);
    let mut x = geom.x;
    let mut y = geom.y;
    let mut width = geom.w;
    let mut height = geom.h;
    let rx = geom.r;

    let transform = attributes
        .get("transform")
        .map(|v| v.to_string())
        .and_then(|s| parse_svg_transform(&s));

    // 描边矩形（`stroke=<实色>` 且 `fill:none`）必须画轮廓，而不是实心 `ROUND_RECT`：
    // 否则边框线宽会被当成实心面，外框退化成填满画布的实心块（表现为背景不透明、
    // 内外边框消失），典型如「画中画」开关的双层矩形框图标。
    let stroked = svg_paint_fill_is_none_like(attributes.get("fill"))
        && svg_paint_stroke_is_usable(attributes.get("stroke"));

    // pattern / 渐变等 paint server 填充无法写入 .icon；跳过以免生成无色的实心占位形（预览呈黑块/黑圆）。
    if !stroked
        && (svg_paint_value_is_url_reference(attributes.get("fill"))
            || svg_paint_value_is_url_reference(attributes.get("stroke")))
    {
        return String::new();
    }

    let inside_mask = if stroked {
        inside_mask_stroke_adjust_for_rect(attributes, masks)
    } else {
        None
    };

    // 内描边 mask：路径内缩至半像素中心（如 3→3.5），配合减半线宽使 Skia 居中
    // 描边与 SVG 内描边视觉一致且栅格清晰。
    if inside_mask.is_some() && transform.is_none() {
        if let Some(adj) = inside_mask {
            x += adj.inset;
            y += adj.inset;
            width -= 2.0 * adj.inset;
            height -= 2.0 * adj.inset;
        }
    }

    if write_new_path {
        output.push_str("NEW_PATH,\r\n");
    }
    if emit_path_colors {
        let color_src = if stroked {
            attributes.get("stroke")
        } else {
            attributes.get("fill")
        };
        if let Some(c) = color_src {
            let color = color_to_argb(&c.to_string());
            if !color.is_empty() {
                output.push_str(&format!("PATH_COLOR_ARGB, {},\r\n", color));
            }
        }
    }

    // 描边模式需在几何命令前声明（与 `handle_svg_path` 的开放折线分支一致）。
    if stroked {
        output.push_str("FILL_RULE_NONZERO,\r\n");
        let sw = inside_mask
            .map(|adj| adj.stroke_width)
            .unwrap_or_else(|| parse_attr_f32(attributes, "stroke-width", 1.0));
        output.push_str(&format!("STROKE, {},\r\n", format_number(sw)));
    }

    if let Some(m) = transform {
        // 旋转/斜切后矩形不再轴对齐，圆角无法用单条命令保留，这里以四边形近似。
        let corners = [
            (x, y),
            (x + width, y),
            (x + width, y + height),
            (x, y + height),
        ];
        let (mx, my) = affine_apply(&m, corners[0].0, corners[0].1);
        output.push_str(&format!(
            "MOVE_TO, {}, {},\r\n",
            format_number(mx),
            format_number(my)
        ));
        for c in &corners[1..] {
            let (px, py) = affine_apply(&m, c.0, c.1);
            output.push_str(&format!(
                "LINE_TO, {}, {},\r\n",
                format_number(px),
                format_number(py)
            ));
        }
        output.push_str("CLOSE,\r\n");
        return output;
    }

    output.push_str(&format!(
        "ROUND_RECT, {}, {}, {}, {}, {},\r\n",
        format_number(x),
        format_number(y),
        format_number(width),
        format_number(height),
        format_number(rx)
    ));
    output
}

/// 输出实心圆盘（`CIRCLE` + 可选 `PATH_COLOR_ARGB`）。
fn emit_chromium_filled_circle(
    cx: f32,
    cy: f32,
    r: f32,
    fill: &Value,
    write_new_path: bool,
    emit_path_colors: bool,
) -> String {
    let mut output = String::new();
    if write_new_path {
        output.push_str("NEW_PATH,\r\n");
    }
    if emit_path_colors {
        let color = color_to_argb(&fill.to_string());
        if !color.is_empty() {
            output.push_str(&format!("PATH_COLOR_ARGB, {},\r\n", color));
        }
    }
    output.push_str(&format!(
        "CIRCLE, {}, {}, {},\r\n",
        format_number(cx),
        format_number(cy),
        format_number(r),
    ));
    output
}

/// 两段闭合折线逼近同心圆，配合 `fill-rule="evenodd"` 得到圆环填充。
fn circle_annulus_path_data(cx: f32, cy: f32, r_outer: f32, r_inner: f32, segments: usize) -> String {
    debug_assert!(r_outer > r_inner && r_inner >= 0.0);
    let seg = segments.max(12);
    let tau = std::f32::consts::TAU;

    let mut outer = String::new();
    let ox = cx + r_outer;
    let oy = cy;
    outer.push_str(&format!("M{} {}", format_number(ox), format_number(oy)));
    for i in 1..seg {
        let a = tau * i as f32 / seg as f32;
        let x = cx + r_outer * a.cos();
        let y = cy + r_outer * a.sin();
        outer.push_str(&format!(" L{} {}", format_number(x), format_number(y)));
    }
    outer.push('Z');

    let mut inner = String::new();
    let ix = cx + r_inner;
    let iy = cy;
    inner.push_str(&format!(" M{} {}", format_number(ix), format_number(iy)));
    for i in 1..seg {
        let a = tau * i as f32 / seg as f32;
        let x = cx + r_inner * a.cos();
        let y = cy + r_inner * a.sin();
        inner.push_str(&format!(" L{} {}", format_number(x), format_number(y)));
    }
    inner.push('Z');

    outer + &inner
}

fn stroke_only_circle_as_evenodd_ring_path(
    attributes: &std::collections::HashMap<String, Value>,
    cx: f32,
    cy: f32,
    r: f32,
    write_new_path: bool,
    emit_path_colors: bool,
) -> Option<String> {
    let stroke = attributes.get("stroke")?;
    let ts = stroke.to_string().trim().to_ascii_lowercase();
    if ts.is_empty() || ts == "none" || ts == "transparent" || ts == "currentcolor" {
        return None;
    }
    let sw = parse_attr_f32(attributes, "stroke-width", 1.0);
    if !(r > 0.0 && sw > 0.0) {
        return None;
    }
    let outer = r + sw * 0.5;
    let inner = (r - sw * 0.5).max(0.0);
    const SEG: usize = 48;
    // 内径过小：描边盖住圆心，等价于实心外圆盘。
    const INNER_EPS: f32 = 2e-3;

    if inner <= INNER_EPS {
        return Some(emit_chromium_filled_circle(
            cx,
            cy,
            outer,
            stroke,
            write_new_path,
            emit_path_colors,
        ));
    }

    let d = circle_annulus_path_data(cx, cy, outer, inner, SEG);
    let mut synth = std::collections::HashMap::new();
    synth.insert("d".into(), Value::from(d));
    synth.insert("fill".into(), stroke.clone());
    synth.insert("fill-rule".into(), Value::from("evenodd"));
    Some(handle_svg_path(&synth, write_new_path, emit_path_colors))
}

/// 把 SVG `<circle>` 转换为 Chromium `CIRCLE` 或（仅 `stroke`、无填充时）近似圆环路径。
fn handle_svg_circle(
    _tag_type: &Type,
    attributes: &std::collections::HashMap<String, Value>,
    write_new_path: bool,
    emit_path_colors: bool,
) -> String {
    let cx = parse_attr_f32(attributes, "cx", 0.0);
    let cy = parse_attr_f32(attributes, "cy", 0.0);
    let r = parse_attr_f32(attributes, "r", 0.0);

    let fill_none = svg_paint_fill_is_none_like(attributes.get("fill"));
    let stroke_ok = svg_paint_stroke_is_usable(attributes.get("stroke"));

    if fill_none && stroke_ok {
        if let Some(ring) = stroke_only_circle_as_evenodd_ring_path(
            attributes,
            cx,
            cy,
            r,
            write_new_path,
            emit_path_colors,
        ) {
            return ring;
        }
        return String::new();
    }

    if fill_none && !stroke_ok {
        return String::new();
    }

    emit_chromium_filled_circle(
        cx,
        cy,
        r,
        attributes.get("fill").unwrap(),
        write_new_path,
        emit_path_colors,
    )
}

/// 把 SVG `<ellipse>` 转换为 Chromium `OVAL, cx, cy, rx, ry,`。
fn handle_svg_ellipse(
    _tag_type: &Type,
    attributes: &std::collections::HashMap<String, Value>,
    write_new_path: bool,
    emit_path_colors: bool,
) -> String {
    let mut output = String::new();

    let cx = parse_attr_f32(attributes, "cx", 0.0);
    let cy = parse_attr_f32(attributes, "cy", 0.0);
    let rx = parse_attr_f32(attributes, "rx", 0.0);
    let ry = parse_attr_f32(attributes, "ry", 0.0);

    if write_new_path {
        output.push_str("NEW_PATH,\r\n");
    }
    if emit_path_colors {
        if let Some(fill) = attributes.get("fill") {
            let color = color_to_argb(&fill.to_string());
            if !color.is_empty() {
                output.push_str(&format!("PATH_COLOR_ARGB, {},\r\n", color));
            }
        }
    }
    output.push_str(&format!(
        "OVAL, {}, {}, {}, {},\r\n",
        format_number(cx),
        format_number(cy),
        format_number(rx),
        format_number(ry),
    ));
    output
}

/// 在转换路径数据时跟踪当前画笔位置以及上一次的控制点，以便
///   * 把相对的 `s`（SmoothCubicCurve Relative）转换为绝对坐标的
///     `CUBIC_TO_SHORTHAND`（Chromium 不存在 `R_CUBIC_TO_SHORTHAND`）；
///   * 让多组参数的复合命令在跟踪上保持一致。
#[derive(Default, Clone, Copy)]
struct PenState {
    cur_x: f32,
    cur_y: f32,
    // 每个子路径起始点（用于 Z/z 之后回到子路径起点）。
    start_x: f32,
    start_y: f32,
}

impl PenState {
    fn move_abs(&mut self, x: f32, y: f32) {
        self.cur_x = x;
        self.cur_y = y;
        self.start_x = x;
        self.start_y = y;
    }

    fn move_rel(&mut self, dx: f32, dy: f32) {
        self.cur_x += dx;
        self.cur_y += dy;
        self.start_x = self.cur_x;
        self.start_y = self.cur_y;
    }

    fn line_abs(&mut self, x: f32, y: f32) {
        self.cur_x = x;
        self.cur_y = y;
    }

    fn line_rel(&mut self, dx: f32, dy: f32) {
        self.cur_x += dx;
        self.cur_y += dy;
    }

    fn close(&mut self) {
        self.cur_x = self.start_x;
        self.cur_y = self.start_y;
    }
}

/// 把已解析的 SVG path `d` 数据追加为 Chromium `MOVE_TO` / `LINE_TO` / … 命令。
fn append_chromium_path_commands_from_data(data: &Data, output: &mut String) {
    let mut pen = PenState::default();
    let mut last_cubic_ctrl: Option<(f32, f32)> = None;

    for command in data.iter() {
        match command {
            Command::Move(position, params) => {
                // SVG 规则：M/m 后多余的坐标对按隐式 LineTo 处理。
                let mut idx = 0;
                let mut first = true;
                while idx + 1 < params.len() {
                    let a = params[idx];
                    let b = params[idx + 1];
                    idx += 2;
                    match position {
                        Position::Absolute => {
                            if first {
                                output.push_str(&format!(
                                    "MOVE_TO, {}, {},\r\n",
                                    format_number(a),
                                    format_number(b)
                                ));
                                pen.move_abs(a, b);
                                first = false;
                            } else {
                                output.push_str(&format!(
                                    "LINE_TO, {}, {},\r\n",
                                    format_number(a),
                                    format_number(b)
                                ));
                                pen.line_abs(a, b);
                            }
                        }
                        Position::Relative => {
                            if first {
                                output.push_str(&format!(
                                    "R_MOVE_TO, {}, {},\r\n",
                                    format_number(a),
                                    format_number(b)
                                ));
                                pen.move_rel(a, b);
                                first = false;
                            } else {
                                output.push_str(&format!(
                                    "R_LINE_TO, {}, {},\r\n",
                                    format_number(a),
                                    format_number(b)
                                ));
                                pen.line_rel(a, b);
                            }
                        }
                    }
                }
                last_cubic_ctrl = None;
            }
            Command::Line(position, params) => {
                let mut idx = 0;
                while idx + 1 < params.len() {
                    let a = params[idx];
                    let b = params[idx + 1];
                    idx += 2;
                    match position {
                        Position::Absolute => {
                            output.push_str(&format!(
                                "LINE_TO, {}, {},\r\n",
                                format_number(a),
                                format_number(b)
                            ));
                            pen.line_abs(a, b);
                        }
                        Position::Relative => {
                            output.push_str(&format!(
                                "R_LINE_TO, {}, {},\r\n",
                                format_number(a),
                                format_number(b)
                            ));
                            pen.line_rel(a, b);
                        }
                    }
                }
                last_cubic_ctrl = None;
            }
            Command::HorizontalLine(position, params) => {
                for &x in params.iter() {
                    match position {
                        Position::Absolute => {
                            output.push_str(&format!("H_LINE_TO, {},\r\n", format_number(x)));
                            pen.line_abs(x, pen.cur_y);
                        }
                        Position::Relative => {
                            output.push_str(&format!("R_H_LINE_TO, {},\r\n", format_number(x)));
                            pen.line_rel(x, 0.0);
                        }
                    }
                }
                last_cubic_ctrl = None;
            }
            Command::VerticalLine(position, params) => {
                for &y in params.iter() {
                    match position {
                        Position::Absolute => {
                            output.push_str(&format!("V_LINE_TO, {},\r\n", format_number(y)));
                            pen.line_abs(pen.cur_x, y);
                        }
                        Position::Relative => {
                            output.push_str(&format!("R_V_LINE_TO, {},\r\n", format_number(y)));
                            pen.line_rel(0.0, y);
                        }
                    }
                }
                last_cubic_ctrl = None;
            }
            Command::QuadraticCurve(position, params) => {
                let mut idx = 0;
                while idx + 3 < params.len() {
                    let (x1, y1, x, y) = (params[idx], params[idx + 1], params[idx + 2], params[idx + 3]);
                    idx += 4;
                    match position {
                        Position::Absolute => {
                            output.push_str(&format!(
                                "QUADRATIC_TO, {}, {}, {}, {},\r\n",
                                format_number(x1),
                                format_number(y1),
                                format_number(x),
                                format_number(y),
                            ));
                            pen.line_abs(x, y);
                        }
                        Position::Relative => {
                            output.push_str(&format!(
                                "R_QUADRATIC_TO, {}, {}, {}, {},\r\n",
                                format_number(x1),
                                format_number(y1),
                                format_number(x),
                                format_number(y),
                            ));
                            pen.line_rel(x, y);
                        }
                    }
                }
                last_cubic_ctrl = None;
            }
            Command::SmoothQuadraticCurve(position, params) => {
                let mut idx = 0;
                while idx + 1 < params.len() {
                    let (x, y) = (params[idx], params[idx + 1]);
                    idx += 2;
                    match position {
                        Position::Absolute => {
                            output.push_str(&format!(
                                "QUADRATIC_TO_SHORTHAND, {}, {},\r\n",
                                format_number(x),
                                format_number(y),
                            ));
                            pen.line_abs(x, y);
                        }
                        Position::Relative => {
                            output.push_str(&format!(
                                "R_QUADRATIC_TO_SHORTHAND, {}, {},\r\n",
                                format_number(x),
                                format_number(y),
                            ));
                            pen.line_rel(x, y);
                        }
                    }
                }
                last_cubic_ctrl = None;
            }
            Command::CubicCurve(position, params) => {
                let mut idx = 0;
                while idx + 5 < params.len() {
                    let (x1, y1, x2, y2, x, y) = (
                        params[idx],
                        params[idx + 1],
                        params[idx + 2],
                        params[idx + 3],
                        params[idx + 4],
                        params[idx + 5],
                    );
                    idx += 6;
                    match position {
                        Position::Absolute => {
                            output.push_str(&format!(
                                "CUBIC_TO, {}, {}, {}, {}, {}, {},\r\n",
                                format_number(x1),
                                format_number(y1),
                                format_number(x2),
                                format_number(y2),
                                format_number(x),
                                format_number(y),
                            ));
                            last_cubic_ctrl = Some((x2, y2));
                            pen.line_abs(x, y);
                        }
                        Position::Relative => {
                            output.push_str(&format!(
                                "R_CUBIC_TO, {}, {}, {}, {}, {}, {},\r\n",
                                format_number(x1),
                                format_number(y1),
                                format_number(x2),
                                format_number(y2),
                                format_number(x),
                                format_number(y),
                            ));
                            last_cubic_ctrl = Some((pen.cur_x + x2, pen.cur_y + y2));
                            pen.line_rel(x, y);
                        }
                    }
                }
            }
            Command::SmoothCubicCurve(position, params) => {
                let mut idx = 0;
                while idx + 3 < params.len() {
                    let (x2, y2, x, y) = (params[idx], params[idx + 1], params[idx + 2], params[idx + 3]);
                    idx += 4;
                    let (abs_x2, abs_y2, abs_x, abs_y) = match position {
                        Position::Absolute => (x2, y2, x, y),
                        Position::Relative => (pen.cur_x + x2, pen.cur_y + y2, pen.cur_x + x, pen.cur_y + y),
                    };
                    output.push_str(&format!(
                        "CUBIC_TO_SHORTHAND, {}, {}, {}, {},\r\n",
                        format_number(abs_x2),
                        format_number(abs_y2),
                        format_number(abs_x),
                        format_number(abs_y),
                    ));
                    last_cubic_ctrl = Some((abs_x2, abs_y2));
                    pen.line_abs(abs_x, abs_y);
                }
            }
            Command::EllipticalArc(position, params) => {
                let mut idx = 0;
                while idx + 6 < params.len() {
                    let (rx, ry, rot, large, sweep, x, y) = (
                        params[idx],
                        params[idx + 1],
                        params[idx + 2],
                        params[idx + 3],
                        params[idx + 4],
                        params[idx + 5],
                        params[idx + 6],
                    );
                    idx += 7;
                    let large_i = if large != 0.0 { 1 } else { 0 };
                    let sweep_i = if sweep != 0.0 { 1 } else { 0 };
                    match position {
                        Position::Absolute => {
                            output.push_str(&format!(
                                "ARC_TO, {}, {}, {}, {}, {}, {}, {},\r\n",
                                format_number(rx),
                                format_number(ry),
                                format_number(rot),
                                large_i,
                                sweep_i,
                                format_number(x),
                                format_number(y),
                            ));
                            pen.line_abs(x, y);
                        }
                        Position::Relative => {
                            output.push_str(&format!(
                                "R_ARC_TO, {}, {}, {}, {}, {}, {}, {},\r\n",
                                format_number(rx),
                                format_number(ry),
                                format_number(rot),
                                large_i,
                                sweep_i,
                                format_number(x),
                                format_number(y),
                            ));
                            pen.line_rel(x, y);
                        }
                    }
                }
                last_cubic_ctrl = None;
            }
            Command::Close => {
                output.push_str("CLOSE,\r\n");
                pen.close();
                last_cubic_ctrl = None;
            }
        }
    }

    let _ = last_cubic_ctrl;
}

/// `fill="none"` 且带 `stroke` 的 `<path>`，用 Chromium 原生 `STROKE` 表达描边。
/// 支持开放折线、含曲线/弧的开放或闭合子路径（如空心五角星 `…Z`）。
///
/// 纯 M/L/H/V 开放折线仍走 [`try_emit_open_stroked_polyline_as_chromium_stroke_commands`]（末尾 `CLOSE`）。
/// 若误把仅描边 path 当填充路径输出，Chromium 会隐式闭合子路径并填充，空心图标会变成实心块。
fn try_emit_stroked_path_as_chromium_stroke_commands(
    attributes: &std::collections::HashMap<String, Value>,
    write_new_path: bool,
    emit_path_colors: bool,
) -> Option<String> {
    if !svg_paint_fill_is_none_like(attributes.get("fill")) {
        return None;
    }
    if !svg_paint_stroke_is_usable(attributes.get("stroke")) {
        return None;
    }
    let d = attributes.get("d")?;
    let parsed = Data::parse(&d.to_string()).ok()?;
    // 纯 M/L/H/V 开放折线由专用分支处理（含末尾 CLOSE）。
    if collect_open_polyline_contours_move_line_only(&parsed).is_some()
        && !parsed.iter().any(|c| matches!(c, Command::Close))
    {
        return None;
    }

    let mut output = String::new();
    if write_new_path {
        output.push_str("NEW_PATH,\r\n");
    }
    if emit_path_colors {
        if let Some(st) = attributes.get("stroke") {
            let color = color_to_argb(&st.to_string());
            if !color.is_empty() {
                output.push_str(&format!("PATH_COLOR_ARGB, {},\r\n", color));
            }
        }
    }
    output.push_str("FILL_RULE_NONZERO,\r\n");
    let sw = parse_attr_f32(attributes, "stroke-width", 1.0);
    output.push_str(&format!("STROKE, {},\r\n", format_number(sw)));
    append_chromium_path_commands_from_data(&parsed, &mut output);
    Some(output)
}

/// 把 SVG `<path>` 的 `d` 属性翻译为一段 Chromium 路径命令。
fn handle_svg_path(
    attributes: &std::collections::HashMap<String, Value>,
    write_new_path: bool,
    emit_path_colors: bool,
) -> String {
    let mut output = String::new();

    if write_new_path {
        output.push_str("NEW_PATH,\r\n");
    }

    if emit_path_colors {
        if let Some(fill) = attributes.get("fill") {
            let color = color_to_argb(&fill.to_string());
            if !color.is_empty() {
                output.push_str(&format!("PATH_COLOR_ARGB, {},\r\n", color));
            }
        }
    }

    // SVG 默认 fill-rule = nonzero；Chromium 默认 evenodd。
    // 因此只在 SVG 是 nonzero（显式或缺省）时输出 FILL_RULE_NONZERO。
    let fill_rule_str = attributes
        .get("fill-rule")
        .map(|v| v.to_string().trim().to_lowercase())
        .unwrap_or_else(|| "nonzero".to_string());
    if fill_rule_str == "nonzero" {
        output.push_str("FILL_RULE_NONZERO,\r\n");
    }

    let data = match attributes.get("d") {
        Some(d) => d,
        None => return output,
    };
    let parsed = match Data::parse(&data.to_string()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[chromium_icon] failed to parse path data: {}", e);
            return output;
        }
    };

    append_chromium_path_commands_from_data(&parsed, &mut output);
    output
}

/// 把指定 SVG 文件转换为 Chromium `.icon` 文本，写到 `output_path`（相对 SVG 所在目录）。
///
/// 返回最终生成的 `.icon` 文件绝对路径字符串。
///
/// 内部调用 [`try_convert_svg_to_chromium_icon`]；任何错误都会被包装成 panic
/// 以保留旧的调用约定。新代码请直接使用返回 `Result` 的版本。
pub fn convert_svg_to_chromium_icon(svg_path: &str, output_path: &str) -> String {
    match try_convert_svg_to_chromium_icon(svg_path, output_path) {
        Ok(path) => path,
        Err(e) => {
            tracing::error!("convert_svg_to_chromium_icon failed: {}", e);
            // 兼容旧调用约定：失败时返回空串而不是 panic，调用方可据此判断。
            String::new()
        }
    }
}

/// SVG → Chromium `.icon` 的可选项（与运行时 `CreateVectorIcon(..., color)` 的配合方式）。
#[derive(Clone, Copy, Debug)]
pub struct SvgToChromiumIconOptions {
    /// 为 `true` 时在几何命令前写入 `PATH_COLOR_ARGB`（常见于带固定色的静态 `.icon`）。
    /// 为 `false` 时不输出颜色列，交由 Chromium 在绘制时用模板 `kColor` 上色。
    pub emit_path_colors: bool,
}

impl Default for SvgToChromiumIconOptions {
    fn default() -> Self {
        Self {
            emit_path_colors: true,
        }
    }
}

/// 出错友好版的 SVG -> .icon 转换。错误会以可阅读的字符串返回，而不是 panic。
pub fn try_convert_svg_to_chromium_icon(
    svg_path: &str,
    output_path: &str,
) -> Result<String, String> {
    try_convert_svg_to_chromium_icon_with_options(
        svg_path,
        output_path,
        &SvgToChromiumIconOptions::default(),
    )
}

/// 出错友好版的 SVG -> .icon 转换（可选是否写入 `PATH_COLOR_ARGB`）。
pub fn try_convert_svg_to_chromium_icon_with_options(
    svg_path: &str,
    output_path: &str,
    options: &SvgToChromiumIconOptions,
) -> Result<String, String> {
    let emit_path_colors = options.emit_path_colors;
    let mut content = String::new();
    let events = svg::open(svg_path, &mut content)
        .map_err(|e| format!("Failed to open/parse SVG '{}': {}", svg_path, e))?
        .collect::<Vec<_>>();

    validate_svg_vector_convertible(&events)?;

    let parent = Path::new(svg_path).parent().unwrap_or_else(|| Path::new("."));
    let dst = PathBuf::from(parent).join(output_path);
    let mut output_file = File::create(dst.clone()).map_err(|e| {
        format!(
            "Failed to create output file '{}': {}",
            dst.display(),
            e
        )
    })?;

    writeln!(output_file, "// Copyright 2015 The Chromium Authors")
        .map_err(|e| format!("Failed to write header: {}", e))?;
    writeln!(
        output_file,
        "// Use of this source code is governed by a BSD-style license that can be"
    )
    .map_err(|e| format!("Failed to write header: {}", e))?;
    writeln!(output_file, "// found in the LICENSE file.")
        .map_err(|e| format!("Failed to write header: {}", e))?;
    writeln!(output_file).map_err(|e| format!("Failed to write header: {}", e))?;

    // 第 1 轮：从 `<svg>` 标签上读取画布尺寸（优先 viewBox，其次 width）。
    // 仅认 Start / Empty 形式的开标签，跳过 End（其 attributes 为空）。
    let mut canvas_dimensions: f64 = 0.0;
    for event in events.iter() {
        if let Event::Tag(name, t, attributes) = event {
            if !matches!(t, Type::Start | Type::Empty) {
                continue;
            }
            if *name == "svg" {
                if let Some(view_box) = attributes.get("viewBox") {
                    if let Some(w) = parse_view_box_width(&view_box.to_string()) {
                        canvas_dimensions = w;
                        break;
                    }
                }
                if let Some(width) = attributes.get("width") {
                    if let Some(w) = parse_dim(width) {
                        canvas_dimensions = w;
                        break;
                    }
                }
            }
        }
    }
    if canvas_dimensions <= 0.0 {
        // 兜底：在任意标签上找 viewBox / width，与旧实现保持兼容。
        for event in events.iter() {
            if let Event::Tag(_, t, attributes) = event {
                if !matches!(t, Type::Start | Type::Empty) {
                    continue;
                }
                if let Some(view_box) = attributes.get("viewBox") {
                    if let Some(w) = parse_view_box_width(&view_box.to_string()) {
                        canvas_dimensions = w;
                        break;
                    }
                } else if let Some(width) = attributes.get("width") {
                    if let Some(w) = parse_dim(width) {
                        canvas_dimensions = w;
                        break;
                    }
                }
            }
        }
    }
    if canvas_dimensions <= 0.0 {
        canvas_dimensions = 24.0; // 与 Material Design 默认尺寸保持一致
    }

    // CANVAS_DIMENSIONS 在 Chromium 端按整数解析（见 ui/gfx/vector_icon_utils.cc 中
    // `ParsePathElement` 对 `kCanvasDimensions` 的 `atoi`），小数会直接被截断或导致
    // 解析失败。这里统一四舍五入，避免 `viewBox="… 464.955 464.955"` 这类非整数
    // 画布尺寸生成出 `CANVAS_DIMENSIONS, 464.95,` 而无法被 Chromium 加载。
    let canvas_int = (canvas_dimensions.round() as i64).max(1);
    writeln!(output_file, "CANVAS_DIMENSIONS, {},", canvas_int)
        .map_err(|e| format!("Failed to write canvas dimensions: {}", e))?;

    // 第 2 轮：依次生成 path/rect/circle/ellipse 命令。
    // 第一个绘制对象不需要 NEW_PATH（隐式），后续每个都需要。
    // 注意：`<defs>` / `<clipPath>` / `<mask>` 等非绘制子树里的形状不参与渲染，
    // 必须整段跳过（含出现在 `<defs>` 之外的 `clipPath`），否则会把剪切白底等画进
    // `.icon` 盖住真实内容。
    //
    // 注意：svg crate 的 `Event::Tag` 第二个字段是 `Type::{Start, End, Empty}`。
    //   * `<path .../>`         -> Empty
    //   * `<path>...</path>`    -> Start + End 两个事件
    //   * `</path>` 等 End tag 的 attributes 是空 HashMap（见 svg crate
    //     `node/element/tag.rs` 中 `Tag(name, Type::End, Attributes::default())`）。
    // 如果对 End tag 也走相同分支会出现：
    //   * path 多写一行 NEW_PATH（attributes 没有 d）；
    //   * rect/circle/ellipse 用默认值 0 生成虚假几何图形。
    // 所以这里要明确忽略 End。
    let is_open_tag = |t: &Type| matches!(t, Type::Start | Type::Empty);

    // 先扫一遍 `<style>` 拿到 CSS 规则，否则像 svgrepo 那种用 class 染色的 SVG
    // 在生成 .icon 时会全部丢失颜色。
    let stylesheet = collect_svg_stylesheet(&events);

    let svg_masks = collect_svg_mask_rects(&events);

    let svg_root_fill: Option<String> = events.iter().find_map(|ev| {
        if let Event::Tag(name, t, attrs) = ev {
            if name.eq_ignore_ascii_case("svg") && matches!(t, Type::Start | Type::Empty) {
                return attrs.get("fill").map(|v| v.to_string());
            }
        }
        None
    });

    let mut non_paint_depth: usize = 0;
    let mut emitted_path = false;
    for event in events.iter() {
        match event {
            Event::Tag(name, t, _) if is_svg_non_paint_subtree(name) => match t {
                Type::Start => non_paint_depth += 1,
                Type::End => non_paint_depth = non_paint_depth.saturating_sub(1),
                Type::Empty => {}
            },
            Event::Tag("g", t, attributes) if is_open_tag(t) && non_paint_depth == 0 => {
                if attributes.contains_key("transform") {
                    eprintln!(
                        "[chromium_icon] warning: <g transform=...> is not supported, \
                         the transform will be ignored. Please flatten transforms in your SVG first."
                    );
                }
            }
            Event::Tag("path", t, attributes) if is_open_tag(t) && non_paint_depth == 0 => {
                let mut resolved = resolve_svg_styles(&stylesheet, attributes, "path");
                let stroke_only_glyph = svg_paint_fill_is_none_like(resolved.get("fill"))
                    && svg_paint_stroke_is_usable(resolved.get("stroke"));

                if stroke_only_glyph {
                    let open_polyline_glyph =
                        !svg_path_data_has_close_command(resolved.get("d"));

                    if open_polyline_glyph {
                        if let Some(chunk) =
                            try_emit_open_stroked_polyline_as_chromium_stroke_commands(
                                &resolved,
                                emitted_path,
                                emit_path_colors,
                            )
                        {
                            write!(output_file, "{}", chunk)
                                .map_err(|e| format!("Failed to write path: {}", e))?;
                            emitted_path = true;
                            continue;
                        }
                    }
                    if let Some(chunk) = try_emit_stroked_path_as_chromium_stroke_commands(
                        &resolved,
                        emitted_path,
                        emit_path_colors,
                    ) {
                        write!(output_file, "{}", chunk)
                            .map_err(|e| format!("Failed to write path: {}", e))?;
                        emitted_path = true;
                        continue;
                    }
                    eprintln!(
                        "[chromium_icon] warning: stroke-only <path> could not be converted to \
                         STROKE commands; skipping to avoid solid-fill artifact"
                    );
                    continue;
                }

                ensure_fill_for_chromium_vector_paint(&mut resolved, svg_root_fill.as_deref());
                let data = handle_svg_path(&resolved, emitted_path, emit_path_colors);
                if !data.is_empty() {
                    write!(output_file, "{}", data)
                        .map_err(|e| format!("Failed to write path: {}", e))?;
                    emitted_path = true;
                }
            }
            Event::Tag("circle", t, attributes) if is_open_tag(t) && non_paint_depth == 0 => {
                let resolved = resolve_svg_styles(&stylesheet, attributes, "circle");
                let data = handle_svg_circle(t, &resolved, emitted_path, emit_path_colors);
                if !data.is_empty() {
                    write!(output_file, "{}", data)
                        .map_err(|e| format!("Failed to write circle: {}", e))?;
                    emitted_path = true;
                }
            }
            Event::Tag("rect", t, attributes) if is_open_tag(t) && non_paint_depth == 0 => {
                let resolved = resolve_svg_styles(&stylesheet, attributes, "rect");
                let data = handle_svg_rect(t, &resolved, emitted_path, emit_path_colors, &svg_masks);
                if !data.is_empty() {
                    write!(output_file, "{}", data)
                        .map_err(|e| format!("Failed to write rect: {}", e))?;
                    emitted_path = true;
                }
            }
            Event::Tag("ellipse", t, attributes) if is_open_tag(t) && non_paint_depth == 0 => {
                let resolved = resolve_svg_styles(&stylesheet, attributes, "ellipse");
                let data = handle_svg_ellipse(t, &resolved, emitted_path, emit_path_colors);
                if !data.is_empty() {
                    write!(output_file, "{}", data)
                        .map_err(|e| format!("Failed to write ellipse: {}", e))?;
                    emitted_path = true;
                }
            }
            _ => {}
        }
    }

    if !emitted_path {
        return Err(
            "SVG 中没有可转换为 Chromium .icon 的矢量图元（可能使用了位图/pattern/渐变填充）。\
             仅支持纯色填充或描边的 path/rect/circle 等矢量形状。"
                .to_string(),
        );
    }

    Ok(dst.to_string_lossy().into_owned())
}

/// 反向转换时的一层 SVG 子节点（路径或 Chromium 基本形）。
enum ReverseIconLayer {
    Path {
        fill_rule: String,
        fill: Option<String>,
        /// `STROKE, w` 之后的子路径；`None` 表示按填充绘制。
        stroke_width: Option<f32>,
        data: Data,
    },
    Circle {
        cx: f32,
        cy: f32,
        r: f32,
        fill: Option<String>,
    },
    Ellipse {
        cx: f32,
        cy: f32,
        rx: f32,
        ry: f32,
        fill: Option<String>,
    },
    RoundRect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        r: f32,
        fill: Option<String>,
        stroke_width: Option<f32>,
    },
}

/// Chromium 未写 `PATH_COLOR_ARGB` 的路径在运行时由 `CreateVectorIcon(..., color)` 上色；
/// 转成静态预览 SVG 时没有该颜色。底层矩形衬底仍为白色，便于 `evenodd` 镂空透出「洞口」底色；
/// 可见几何若仍无嵌入色则用深默认灰，与白底预览对比清晰。
const REVERSE_ICON_CANVAS_BACKDROP_FILL: &str = "#ffffff";
const REVERSE_ICON_CANVAS_BACKDROP_DARK: &str = "#111111";
/// 无嵌入 `PATH_COLOR` 的路径 / `CIRCLE` / `ROUND_RECT` 等在预览 SVG 中的默认填充。
const REVERSE_ICON_DEFAULT_SHAPE_FILL: &str = "#424242";

/// 批量/单图 SVG 预览衬底：`light`（默认白）或 `dark`（黑）。
pub fn icon_svg_preview_backdrop_fill(bg: Option<&str>) -> &'static str {
    match bg
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("dark" | "black") => REVERSE_ICON_CANVAS_BACKDROP_DARK,
        _ => REVERSE_ICON_CANVAS_BACKDROP_FILL,
    }
}

/// 规范化 `.icon` 行首命令名：去 BOM、首尾空白，并只保留 `A–Z` / `0–9` / `_` 前缀。
///
/// 部分工具或剪贴板会在 `NEW_PATH` 等词后附带不可见 Unicode（如 U+200E），会导致
/// 与字面 `"NEW_PATH"` 匹配失败；这里与 Chromium 实际使用的 ASCII 命令名对齐。
fn strip_icon_command_token(raw: &str) -> String {
    let s = raw.trim().trim_start_matches('\u{feff}');
    s.chars()
        .take_while(|c| matches!(*c, 'A'..='Z' | '0'..='9' | '_'))
        .collect()
}

/// 解析 `.icon` 行内浮点坐标。仅允许 **单个** 尾部 `f`/`F`（旧式 `1.5f`），不得对整段做 `trim_end_matches('f')`，
/// 否则会破坏 `PATH_COLOR_ARGB` 里的 `0xff` 等十六进制字面量。
fn parse_icon_f32_token(s: &str) -> f32 {
    let s = s.trim();
    if let Ok(v) = s.parse::<f32>() {
        return v;
    }
    if s.len() > 1 {
        let b = s.as_bytes();
        let last = b[b.len() - 1];
        if last == b'f' || last == b'F' {
            if let Ok(v) = s[..s.len() - 1].parse::<f32>() {
                return v;
            }
        }
    }
    0.0
}

fn flush_icon_path_layer(
    data: Data,
    path_nonempty: bool,
    fill_rule: &str,
    fill: &Option<String>,
    stroke_width: Option<f32>,
    out: &mut Vec<ReverseIconLayer>,
) {
    if !path_nonempty {
        return;
    }
    out.push(ReverseIconLayer::Path {
        fill_rule: fill_rule.to_string(),
        fill: fill.clone(),
        stroke_width,
        data,
    });
}

/// 把 Chromium `.icon` 源文本反向解析为 SVG 字符串（供 `<img src>` 等预览）。
///
/// 支持 `NEW_PATH` 多子路径、`CIRCLE` / `OVAL` / `ROUND_RECT` 与路径命令混合，
/// 与正向 `handle_svg_*` 输出对齐，避免信封等形状在预览中丢失。
pub fn try_convert_chromium_icon_source_to_svg_markup(source: &str) -> Result<String, String> {
    try_convert_chromium_icon_source_to_svg_markup_with_backdrop(
        source,
        REVERSE_ICON_CANVAS_BACKDROP_FILL,
    )
}

pub fn try_convert_chromium_icon_source_to_svg_markup_with_backdrop(
    source: &str,
    canvas_backdrop_fill: &str,
) -> Result<String, String> {
    let mut layers: Vec<ReverseIconLayer> = Vec::new();
    let mut path_data = Data::new();
    let mut path_nonempty = false;
    let mut canvas_dimensions: u32 = 24;
    let mut fill_rule = "evenodd".to_string();
    let mut fill_color: Option<String> = None;
    // 当前子路径上最近一次 `STROKE, w`；`NEW_PATH` 后 Chromium 会复位绘制标志，此处同步清空。
    let mut vector_stroke_width: Option<f32> = None;
    let mut pen = PenState::default();

    for line in source.lines() {
        let trimmed = line.trim();
        let stripped = match trimmed.find("//") {
            Some(i) => trimmed[..i].trim(),
            None => trimmed,
        };
        if stripped.is_empty() {
            continue;
        }

        let parts: Vec<String> = stripped
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if parts.is_empty() {
            continue;
        }

        let cmd = strip_icon_command_token(&parts[0]);
        if cmd.is_empty() {
            continue;
        }
        let pf = |i: usize| -> f32 {
            parts
                .get(i)
                .map(|s| parse_icon_f32_token(s))
                .unwrap_or(0.0)
        };
        let pi = |i: usize| -> i64 {
            parts
                .get(i)
                .map(|s| {
                    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                        i64::from_str_radix(hex, 16).unwrap_or(0)
                    } else {
                        s.parse::<i64>().unwrap_or(0)
                    }
                })
                .unwrap_or(0)
        };

        match cmd.as_str() {
            "CANVAS_DIMENSIONS" => {
                // 与正向输出对齐：Chromium 这里期望整数，遇到历史遗留的小数也四舍五入。
                let raw = pf(1);
                if raw > 0.0 {
                    canvas_dimensions = raw.round().max(1.0) as u32;
                }
            }
            "FILL_RULE_NONZERO" => {
                fill_rule = "nonzero".to_string();
            }
            "PATH_COLOR_ARGB" => {
                let a = pi(1) as u32;
                let r = pi(2) as u32;
                let g = pi(3) as u32;
                let b = pi(4) as u32;
                if a == 0xFF {
                    fill_color = Some(format!("#{:02X}{:02X}{:02X}", r, g, b));
                } else {
                    fill_color = Some(format!("#{:02X}{:02X}{:02X}{:02X}", r, g, b, a));
                }
            }
            "NEW_PATH" => {
                let stroke_snapshot = vector_stroke_width.take();
                let old = std::mem::replace(&mut path_data, Data::new());
                let had = path_nonempty;
                path_nonempty = false;
                flush_icon_path_layer(
                    old,
                    had,
                    &fill_rule,
                    &fill_color,
                    stroke_snapshot,
                    &mut layers,
                );
                pen = PenState::default();
            }
            "CIRCLE" => {
                let stroke_snapshot = vector_stroke_width.take();
                let old = std::mem::replace(&mut path_data, Data::new());
                let had = path_nonempty;
                path_nonempty = false;
                flush_icon_path_layer(
                    old,
                    had,
                    &fill_rule,
                    &fill_color,
                    stroke_snapshot,
                    &mut layers,
                );
                pen = PenState::default();
                layers.push(ReverseIconLayer::Circle {
                    cx: pf(1),
                    cy: pf(2),
                    r: pf(3),
                    fill: fill_color.clone(),
                });
            }
            "OVAL" => {
                let stroke_snapshot = vector_stroke_width.take();
                let old = std::mem::replace(&mut path_data, Data::new());
                let had = path_nonempty;
                path_nonempty = false;
                flush_icon_path_layer(
                    old,
                    had,
                    &fill_rule,
                    &fill_color,
                    stroke_snapshot,
                    &mut layers,
                );
                pen = PenState::default();
                layers.push(ReverseIconLayer::Ellipse {
                    cx: pf(1),
                    cy: pf(2),
                    rx: pf(3),
                    ry: pf(4),
                    fill: fill_color.clone(),
                });
            }
            "ROUND_RECT" => {
                let stroke_snapshot = vector_stroke_width.take();
                let old = std::mem::replace(&mut path_data, Data::new());
                let had = path_nonempty;
                path_nonempty = false;
                // 若 `STROKE` 前已有挂起路径，描边属于该路径；否则描边作用于本 ROUND_RECT。
                let (path_stroke, rect_stroke) = if had {
                    (stroke_snapshot, None)
                } else {
                    (None, stroke_snapshot)
                };
                flush_icon_path_layer(
                    old,
                    had,
                    &fill_rule,
                    &fill_color,
                    path_stroke,
                    &mut layers,
                );
                pen = PenState::default();
                layers.push(ReverseIconLayer::RoundRect {
                    x: pf(1),
                    y: pf(2),
                    w: pf(3),
                    h: pf(4),
                    r: pf(5),
                    fill: fill_color.clone(),
                    stroke_width: rect_stroke,
                });
            }
            "MOVE_TO" => {
                let (x, y) = (pf(1), pf(2));
                path_data = path_data.move_to((x, y));
                pen.move_abs(x, y);
                path_nonempty = true;
            }
            "R_MOVE_TO" => {
                let (x, y) = (pf(1), pf(2));
                path_data = path_data.move_by((x, y));
                pen.move_rel(x, y);
                path_nonempty = true;
            }
            "LINE_TO" => {
                let (x, y) = (pf(1), pf(2));
                path_data = path_data.line_to((x, y));
                pen.line_abs(x, y);
                path_nonempty = true;
            }
            "R_LINE_TO" => {
                let (x, y) = (pf(1), pf(2));
                path_data = path_data.line_by((x, y));
                pen.line_rel(x, y);
                path_nonempty = true;
            }
            "H_LINE_TO" => {
                let x = pf(1);
                path_data = path_data.horizontal_line_to(x);
                pen.line_abs(x, pen.cur_y);
                path_nonempty = true;
            }
            "R_H_LINE_TO" => {
                let x = pf(1);
                path_data = path_data.horizontal_line_by(x);
                pen.line_rel(x, 0.0);
                path_nonempty = true;
            }
            "V_LINE_TO" => {
                let y = pf(1);
                path_data = path_data.vertical_line_to(y);
                pen.line_abs(pen.cur_x, y);
                path_nonempty = true;
            }
            "R_V_LINE_TO" => {
                let y = pf(1);
                path_data = path_data.vertical_line_by(y);
                pen.line_rel(0.0, y);
                path_nonempty = true;
            }
            "QUADRATIC_TO" => {
                let (x1, y1, x, y) = (pf(1), pf(2), pf(3), pf(4));
                path_data = path_data.quadratic_curve_to((x1, y1, x, y));
                pen.line_abs(x, y);
                path_nonempty = true;
            }
            "R_QUADRATIC_TO" => {
                let (x1, y1, x, y) = (pf(1), pf(2), pf(3), pf(4));
                path_data = path_data.quadratic_curve_by((x1, y1, x, y));
                pen.line_rel(x, y);
                path_nonempty = true;
            }
            "QUADRATIC_TO_SHORTHAND" => {
                let (x, y) = (pf(1), pf(2));
                path_data = path_data.smooth_quadratic_curve_to((x, y));
                pen.line_abs(x, y);
                path_nonempty = true;
            }
            "R_QUADRATIC_TO_SHORTHAND" => {
                let (x, y) = (pf(1), pf(2));
                path_data = path_data.smooth_quadratic_curve_by((x, y));
                pen.line_rel(x, y);
                path_nonempty = true;
            }
            "CUBIC_TO" => {
                let (x1, y1, x2, y2, x, y) = (pf(1), pf(2), pf(3), pf(4), pf(5), pf(6));
                path_data = path_data.cubic_curve_to((x1, y1, x2, y2, x, y));
                pen.line_abs(x, y);
                path_nonempty = true;
            }
            "R_CUBIC_TO" => {
                let (x1, y1, x2, y2, x, y) = (pf(1), pf(2), pf(3), pf(4), pf(5), pf(6));
                path_data = path_data.cubic_curve_by((x1, y1, x2, y2, x, y));
                pen.line_rel(x, y);
                path_nonempty = true;
            }
            "CUBIC_TO_SHORTHAND" => {
                let (x2, y2, x, y) = (pf(1), pf(2), pf(3), pf(4));
                path_data = path_data.smooth_cubic_curve_to((x2, y2, x, y));
                pen.line_abs(x, y);
                path_nonempty = true;
            }
            "ARC_TO" => {
                let rx = pf(1);
                let ry = pf(2);
                let rot = pf(3);
                let large = if pi(4) != 0 { 1.0 } else { 0.0 };
                let sweep = if pi(5) != 0 { 1.0 } else { 0.0 };
                let x = pf(6);
                let y = pf(7);
                path_data = path_data.elliptical_arc_to((rx, ry, rot, large, sweep, x, y));
                pen.line_abs(x, y);
                path_nonempty = true;
            }
            "R_ARC_TO" => {
                let rx = pf(1);
                let ry = pf(2);
                let rot = pf(3);
                let large = if pi(4) != 0 { 1.0 } else { 0.0 };
                let sweep = if pi(5) != 0 { 1.0 } else { 0.0 };
                let x = pf(6);
                let y = pf(7);
                path_data = path_data.elliptical_arc_by((rx, ry, rot, large, sweep, x, y));
                pen.line_rel(x, y);
                path_nonempty = true;
            }
            "CLOSE" => {
                path_data = path_data.close();
                pen.close();
                path_nonempty = true;
            }
            "STROKE" => {
                vector_stroke_width = Some(pf(1));
            }
            "PATH_COLOR_ALPHA" | "PATH_MODE_CLEAR" | "CAP_SQUARE" | "CLIP" | "DISABLE_AA"
            | "FLIPS_IN_RTL" => {
                tracing::debug!(
                    target: "chromium_icon",
                    command = %cmd,
                    "reverse: skip optional vector command"
                );
            }
            _ => {
                tracing::warn!(
                    target: "chromium_icon",
                    command = %cmd,
                    "reverse: unknown vector command"
                );
            }
        }
    }

    flush_icon_path_layer(
        path_data,
        path_nonempty,
        &fill_rule,
        &fill_color,
        vector_stroke_width.take(),
        &mut layers,
    );

    // `.icon` 只存 `CANVAS_DIMENSIONS`（设计坐标 / viewBox），不存渲染尺寸；
    // Chromium 在运行时由 `CreateVectorIcon(.., size, ..)` 决定 px。
    //
    // 仍然写出 width/height = canvas_dimensions：浏览器把外部 SVG 装入 `<img>` 时
    // 不解析 viewBox，缺失 width/height 会被认为「无内在尺寸」从而被 CSS `width:auto`
    // 算成 0×0，导致灯箱里大图不可见。这里给一个最自然的内在尺寸（= 设计坐标），
    // 由调用方/容器再按需缩放即可。
    let mut doc = Document::new()
        .set("xmlns", "http://www.w3.org/2000/svg")
        .set("viewBox", (0u32, 0u32, canvas_dimensions, canvas_dimensions))
        .set("width", canvas_dimensions)
        .set("height", canvas_dimensions)
        .set("preserveAspectRatio", "xMidYMid meet")
        .set("fill-rule", "evenodd");
    // 底层衬底：使 evenodd 子路径形成的「洞」透出白色，而不是透明（在深色背景/`<img>` 下呈黑）。
    doc = doc.add(
        Rectangle::new()
            .set("x", 0)
            .set("y", 0)
            .set("width", canvas_dimensions)
            .set("height", canvas_dimensions)
            .set("fill", canvas_backdrop_fill),
    );

    for layer in layers {
        doc = match layer {
            ReverseIconLayer::Path {
                fill_rule,
                fill,
                stroke_width,
                data,
            } => {
                let mut p = SvgPath::new()
                    .set("fill-rule", fill_rule.as_str())
                    .set("d", data);
                if let Some(sw) = stroke_width {
                    let stroke_paint = fill
                        .clone()
                        .unwrap_or_else(|| REVERSE_ICON_DEFAULT_SHAPE_FILL.to_string());
                    p = p
                        .set("fill", "none")
                        .set("stroke", stroke_paint.as_str())
                        .set("stroke-width", sw)
                        .set("stroke-linecap", "round")
                        .set("stroke-linejoin", "round");
                } else if let Some(ref c) = fill {
                    p = p.set("fill", c.as_str());
                } else {
                    p = p.set("fill", REVERSE_ICON_DEFAULT_SHAPE_FILL);
                }
                doc.add(p)
            }
            ReverseIconLayer::Circle { cx, cy, r, fill } => {
                let mut c = Circle::new()
                    .set("cx", cx)
                    .set("cy", cy)
                    .set("r", r);
                c = if let Some(ref f) = fill {
                    c.set("fill", f.as_str())
                } else {
                    c.set("fill", REVERSE_ICON_DEFAULT_SHAPE_FILL)
                };
                doc.add(c)
            }
            ReverseIconLayer::Ellipse {
                cx,
                cy,
                rx,
                ry,
                fill,
            } => {
                let mut e = Ellipse::new()
                    .set("cx", cx)
                    .set("cy", cy)
                    .set("rx", rx)
                    .set("ry", ry);
                e = if let Some(ref f) = fill {
                    e.set("fill", f.as_str())
                } else {
                    e.set("fill", REVERSE_ICON_DEFAULT_SHAPE_FILL)
                };
                doc.add(e)
            }
            ReverseIconLayer::RoundRect {
                x,
                y,
                w,
                h,
                r,
                fill,
                stroke_width,
            } => {
                let mut rect = Rectangle::new()
                    .set("x", x)
                    .set("y", y)
                    .set("width", w)
                    .set("height", h);
                if r > 0.0 {
                    rect = rect.set("rx", r).set("ry", r);
                }
                rect = if let Some(sw) = stroke_width {
                    let stroke_paint = fill
                        .clone()
                        .unwrap_or_else(|| REVERSE_ICON_DEFAULT_SHAPE_FILL.to_string());
                    rect.set("fill", "none")
                        .set("stroke", stroke_paint.as_str())
                        .set("stroke-width", sw)
                } else if let Some(ref f) = fill {
                    rect.set("fill", f.as_str())
                } else {
                    rect.set("fill", REVERSE_ICON_DEFAULT_SHAPE_FILL)
                };
                doc.add(rect)
            }
        };
    }

    Ok(doc.to_string())
}

/// 从磁盘上的 `.icon` 文件生成 SVG 字符串（浏览器 `<img src="...svg">` 预览用）。
pub fn try_convert_chromium_icon_path_to_svg_markup(icon_path: &str) -> Result<String, String> {
    try_convert_chromium_icon_path_to_svg_markup_with_backdrop(icon_path, None)
}

pub fn try_convert_chromium_icon_path_to_svg_markup_with_backdrop(
    icon_path: &str,
    preview_bg: Option<&str>,
) -> Result<String, String> {
    let source =
        std::fs::read_to_string(icon_path).map_err(|e| format!("Failed to read icon file: {}", e))?;
    try_convert_chromium_icon_source_to_svg_markup_with_backdrop(
        &source,
        icon_svg_preview_backdrop_fill(preview_bg),
    )
}

/// 把 Chromium `.icon` 文件反向解析为一个 SVG 文件（用于预览或调试）。
#[allow(dead_code)]
pub fn convert_chromium_icon_to_svg(icon_path: &str, output_path: &str) {
    let markup = try_convert_chromium_icon_path_to_svg_markup(icon_path)
        .unwrap_or_else(|e| panic!("Failed to convert icon to SVG: {}", e));
    std::fs::write(output_path, markup.as_bytes()).expect("Failed to save SVG file");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_number_basic() {
        assert_eq!(format_number(0.0), "0");
        assert_eq!(format_number(1.0), "1");
        assert_eq!(format_number(-1.0), "-1");
        assert_eq!(format_number(1.5), "1.5");
        assert_eq!(format_number(-1.5), "-1.5");
        assert_eq!(format_number(0.5), "0.5");
        assert_eq!(format_number(0.25), "0.25");
        // -0.97 距 -1 仅 0.03（≤ 吸附阈值）→ 吸附到整数 -1。
        assert_eq!(format_number(-0.97), "-1");
    }

    #[test]
    fn format_number_snaps_near_integer_and_rounds() {
        // 浮点噪声/导出误差吸附到整数网格，规避非整数发虚。
        assert_eq!(format_number(11.9998), "12");
        assert_eq!(format_number(2.0001), "2");
        assert_eq!(format_number(-0.0001), "0");
        assert_eq!(format_number(7.04), "7");
        // 有意的半像素描边（.5 居中）必须保留。
        assert_eq!(format_number(1.5), "1.5");
        assert_eq!(format_number(2.5), "2.5");
        // 远离整数的真实分数：四舍五入到 2 位（而非截断）。
        assert_eq!(format_number(6.11084), "6.11");
        assert_eq!(format_number(6.699), "6.7");
        assert_eq!(format_number(0.125), "0.13");
    }

    #[test]
    fn format_number_no_f_suffix() {
        // 与现有 chromium .icon 文件保持一致：不带 f 后缀
        assert!(!format_number(1.5).ends_with('f'));
        assert!(!format_number(-0.97).ends_with('f'));
    }

    #[test]
    fn parse_icon_f32_token_strips_float_suffix_only() {
        assert!((parse_icon_f32_token("554.21") - 554.21).abs() < 1e-3);
        assert!((parse_icon_f32_token("1.5f") - 1.5).abs() < 1e-3);
        // `0xff` 是十六进制颜色分量，不能当浮点；本函数返回 0（颜色由 `pi` 解析）。
        assert_eq!(parse_icon_f32_token("0xff"), 0.0);
    }

    #[test]
    fn reverse_icon_split_preserves_path_color_hex_tokens() {
        let line = "PATH_COLOR_ARGB, 0xFF, 0xff, 0xa9, 0xb1,";
        let parts: Vec<String> = line
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(parts[2], "0xff");
        let s = parts[2].as_str();
        let v = if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            i64::from_str_radix(hex, 16).unwrap()
        } else {
            0
        };
        assert_eq!(v, 255);
    }

    #[test]
    fn reverse_icon_path_color_argb_preserves_red_channel() {
        // 这是 Chromium 标准的 ARGB 颜色行；R 字段恰好是 0xff，
        // 之前 `trim_end_matches('f')` 会把它毁成 0x，导致 R=0。
        let icon = "CANVAS_DIMENSIONS, 24,\nPATH_COLOR_ARGB, 0xFF, 0xff, 0xa9, 0xb1,\nMOVE_TO, 0, 0,\nLINE_TO, 1, 1,\nCLOSE,\n";
        let svg =
            try_convert_chromium_icon_source_to_svg_markup(icon).expect("should convert");
        assert!(
            svg.contains("#FFA9B1"),
            "expected #FFA9B1 in svg, got: {}",
            svg
        );
        assert!(
            !svg.contains("#00A9B1"),
            "should NOT contain #00A9B1 (red channel lost), got: {}",
            svg
        );
    }

    #[test]
    fn color_named_green_is_dark_green() {
        // CSS 标准里 "green" 是 #008000，不是 #00FF00
        assert_eq!(color_to_argb("green"), "0xFF, 0x00, 0x80, 0x00");
        assert_eq!(color_to_argb("lime"), "0xFF, 0x00, 0xFF, 0x00");
    }

    #[test]
    fn color_hex_short() {
        assert_eq!(color_to_argb("#abc"), "0xFF, 0xaa, 0xbb, 0xcc");
        assert_eq!(color_to_argb("#abcd"), "0xdd, 0xaa, 0xbb, 0xcc");
    }

    #[test]
    fn color_hex_long() {
        assert_eq!(color_to_argb("#112233"), "0xFF, 0x11, 0x22, 0x33");
        assert_eq!(color_to_argb("#11223344"), "0x44, 0x11, 0x22, 0x33");
    }

    #[test]
    fn color_none_or_unknown() {
        assert_eq!(color_to_argb("none"), "");
        assert_eq!(color_to_argb(""), "");
    }

    #[test]
    fn parse_view_box_with_commas_and_spaces() {
        assert_eq!(parse_view_box_width("0 0 24 24"), Some(24.0));
        assert_eq!(parse_view_box_width("0,0,24,24"), Some(24.0));
        assert_eq!(parse_view_box_width("0 -960 960 960"), Some(960.0));
    }

    #[test]
    fn css_strip_comments_basic() {
        assert_eq!(strip_css_comments("a/* x */b/*y*/c"), "abc");
        assert_eq!(strip_css_comments("a/* unterminated"), "a");
        assert_eq!(strip_css_comments("plain"), "plain");
    }

    #[test]
    fn css_parse_class_and_tag_selectors() {
        let sheet = parse_svg_css(".a{fill:#ffffff;}.b{fill:#211715}path{fill-rule:evenodd}");
        assert_eq!(sheet.get(".a").unwrap().get("fill").unwrap(), "#ffffff");
        assert_eq!(sheet.get(".b").unwrap().get("fill").unwrap(), "#211715");
        assert_eq!(sheet.get("path").unwrap().get("fill-rule").unwrap(), "evenodd");
    }

    #[test]
    fn reverse_icon_without_path_color_uses_dark_gray_preview_fill() {
        let icon = "CANVAS_DIMENSIONS, 16,\n\
MOVE_TO, 0, 0,\nLINE_TO, 16, 0,\nLINE_TO, 16, 16,\nLINE_TO, 0, 16,\nCLOSE,\n";
        let svg = try_convert_chromium_icon_source_to_svg_markup(icon).expect("convert");
        assert!(
            svg.contains("#424242"),
            "missing default dark-gray fill for no PATH_COLOR geometry: {}",
            svg
        );
        assert!(
            svg.contains("#ffffff") || svg.contains("#FFFFFF"),
            "canvas backdrop should stay white for evenodd holes: {}",
            svg
        );
    }

    #[test]
    fn css_parse_grouped_selectors_and_comments() {
        let sheet = parse_svg_css("/* head */ .a, .b { fill: red; } /* tail */");
        assert_eq!(sheet.get(".a").unwrap().get("fill").unwrap(), "red");
        assert_eq!(sheet.get(".b").unwrap().get("fill").unwrap(), "red");
    }

    /// 端到端：用 svgrepo 风格（CSS 类染色）的最小 SVG 走一次正向 + 反向，
    /// 确保 `.icon` 里有 `PATH_COLOR_ARGB`，反向 SVG 含目标颜色。
    /// 之前会丢色，导致预览整张图变成纯白。
    #[test]
    fn round_trip_class_styled_svg_keeps_color() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!(
            "chromium_icon_test_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let svg_path = dir.join("class_styled.svg");
        let icon_name = "class_styled.icon";

        let svg = r#"<?xml version="1.0"?>
<svg width="24" height="24" viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg">
  <defs><style>.a{fill:#ffa9b1;}.b{fill:#211715;}</style></defs>
  <path class="a" d="M0,0 L10,0 L10,10 Z"/>
  <path class="b" d="M12,12 L20,12 L20,20 Z"/>
</svg>
"#;
        let mut f = std::fs::File::create(&svg_path).unwrap();
        f.write_all(svg.as_bytes()).unwrap();
        drop(f);

        let icon_path = try_convert_svg_to_chromium_icon(svg_path.to_str().unwrap(), icon_name)
            .expect("svg -> icon should succeed");
        let icon_text = std::fs::read_to_string(&icon_path).unwrap();
        assert!(
            icon_text.contains("PATH_COLOR_ARGB, 0xFF, 0xff, 0xa9, 0xb1,"),
            "missing class .a color in .icon, got: {}",
            icon_text
        );
        assert!(
            icon_text.contains("PATH_COLOR_ARGB, 0xFF, 0x21, 0x17, 0x15,"),
            "missing class .b color in .icon, got: {}",
            icon_text
        );

        let svg_back = try_convert_chromium_icon_path_to_svg_markup(&icon_path)
            .expect("icon -> svg should succeed");
        assert!(
            svg_back.contains("#FFA9B1"),
            "reverse svg lost class .a color, got: {}",
            svg_back
        );
        assert!(
            svg_back.contains("#211715"),
            "reverse svg lost class .b color, got: {}",
            svg_back
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_transform_rotate_about_point() {
        let m = parse_svg_transform("rotate(45 6.11084 5.4043)").expect("should parse");
        // 旋转中心点保持不动。
        let (px, py) = affine_apply(&m, 6.11084, 5.4043);
        assert!((px - 6.11084).abs() < 1e-3, "cx moved: {}", px);
        assert!((py - 5.4043).abs() < 1e-3, "cy moved: {}", py);
    }

    /// 旋转矩形拼出的「X」关闭图标：每个 `<rect transform="rotate(..)">` 必须输出为
    /// 旋转后的四边形路径（MOVE_TO/LINE_TO），不能退化成轴对齐的 ROUND_RECT 横杠。
    #[test]
    fn rotated_rect_emits_quad_path_not_round_rect() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("chromium_icon_rotrect_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let svg_path = dir.join("x_close.svg");

        let svg = r#"<svg width="20" height="20" viewBox="0 0 20 20" fill="none" xmlns="http://www.w3.org/2000/svg">
<rect x="6.11084" y="5.4043" width="12" height="1" transform="rotate(45 6.11084 5.4043)" fill="white"/>
<rect x="14.5961" y="6.11133" width="12" height="1" transform="rotate(135 14.5961 6.11133)" fill="white"/>
</svg>
"#;
        let mut f = std::fs::File::create(&svg_path).unwrap();
        f.write_all(svg.as_bytes()).unwrap();
        drop(f);

        let icon_path = try_convert_svg_to_chromium_icon(svg_path.to_str().unwrap(), "x_close.icon")
            .expect("svg -> icon should succeed");
        let icon_text = std::fs::read_to_string(&icon_path).unwrap();
        assert!(
            !icon_text.contains("ROUND_RECT"),
            "rotated rect must not become axis-aligned ROUND_RECT:\n{}",
            icon_text
        );
        assert_eq!(
            icon_text.matches("MOVE_TO").count(),
            2,
            "expected two quad subpaths for the X strokes:\n{}",
            icon_text
        );
        assert!(
            icon_text.contains("LINE_TO") && icon_text.contains("CLOSE"),
            "rotated rect should emit a closed quad path:\n{}",
            icon_text
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 描边矩形（`stroke` + `fill="none"`，如画中画开关的双层框）必须用 `STROKE` 画轮廓，
    /// 不能输出成实心 `ROUND_RECT`（否则外框退化成填满画布的实心块、边框消失）。
    #[test]
    fn stroked_rect_emits_stroke_outline_not_solid_fill() {
        use std::io::Write;
        let dir =
            std::env::temp_dir().join(format!("chromium_icon_strokerect_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let svg_path = dir.join("pip.svg");

        let svg = r#"<svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
<rect x="1.5" y="2.5" width="13" height="11" rx="0.5" stroke="white"/>
<rect x="7.5" y="7.5" width="5" height="4" rx="0.6" stroke="white"/>
<path d="M7 4V5H4V8H3V4H7Z" fill="white"/>
</svg>
"#;
        let mut f = std::fs::File::create(&svg_path).unwrap();
        f.write_all(svg.as_bytes()).unwrap();
        drop(f);

        let icon_path = try_convert_svg_to_chromium_icon(svg_path.to_str().unwrap(), "pip.icon")
            .expect("svg -> icon should succeed");
        let icon_text = std::fs::read_to_string(&icon_path).unwrap();
        // 两个描边框都必须带 STROKE 命令。
        assert_eq!(
            icon_text.matches("STROKE,").count(),
            2,
            "both stroked rects must emit STROKE:\n{}",
            icon_text
        );
        assert!(
            icon_text.contains("ROUND_RECT, 1.5, 2.5, 13, 11, 0.5,"),
            "outer rounded rect geometry missing:\n{}",
            icon_text
        );

        // 反向预览：描边框应渲染为 stroke 轮廓（fill:none），而非实心填充。
        let svg_back = try_convert_chromium_icon_path_to_svg_markup(&icon_path)
            .expect("icon -> svg should succeed");
        assert!(
            svg_back.contains("stroke-width"),
            "reverse preview lost stroke outline:\n{}",
            svg_back
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 书签空心星（闭合 path + 仅 stroke）须走 `STROKE`，不能当填充路径（会变成实心星）。
    #[test]
    fn stroke_only_closed_star_emits_stroke_not_solid_fill() {
        let dir = std::env::temp_dir().join(format!(
            "chromium_icon_bookmark_star_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let svg_path = dir.join("ic_bookmarks_default.svg");
        let svg = br##"<svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
<path d="M7.54996 0.929695C7.73206 0.553502 8.26794 0.553503 8.45004 0.929696L10.4227 5.00478L14.9487 5.59289C15.3679 5.64735 15.5346 6.16502 15.2259 6.45381L11.92 9.54733L12.7517 13.9747C12.8292 14.3872 12.3945 14.7059 12.0244 14.5079L8 12.3548L3.97561 14.5079C3.60548 14.7059 3.17084 14.3872 3.24833 13.9747L4.08 9.54733L0.774057 6.45381C0.465439 6.16502 0.63212 5.64735 1.05126 5.59289L5.57731 5.00478L7.54996 0.929695Z" stroke="#1F2530"/>
</svg>"##;
        std::fs::write(&svg_path, svg).unwrap();
        let icon_path = try_convert_svg_to_chromium_icon(
            svg_path.to_str().unwrap(),
            "ic_bookmarks_default.icon",
        )
        .unwrap();
        let txt = std::fs::read_to_string(&icon_path).unwrap();
        assert!(
            txt.contains("STROKE,"),
            "hollow star must use STROKE, not fill:\n{}",
            txt
        );
        assert!(
            txt.contains("PATH_COLOR_ARGB, 0xFF, 0x1F, 0x25, 0x30,"),
            "stroke color must be emitted:\n{}",
            txt
        );
        assert!(
            txt.contains("CLOSE,"),
            "closed star path must retain CLOSE:\n{}",
            txt
        );
        assert!(
            !txt.contains("CUBIC_TO,") || txt.contains("STROKE,"),
            "unexpected solid-fill path without STROKE:\n{}",
            txt
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 还原窗口图标：后层 L 形描边含圆角 cubic，必须是 `STROKE` 开放路径，不能当填充
    /// （否则隐式闭合会在右上角出现实心三角）。
    #[test]
    fn restore_icon_open_stroked_path_with_cubic_emits_stroke_not_fill() {
        use std::io::Write;
        let dir =
            std::env::temp_dir().join(format!("chromium_icon_restore_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let svg_path = dir.join("icon-restore.svg");

        let svg = r##"<svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
<rect x="2.5" y="4.5" width="9" height="9" rx="0.5" stroke="#1F2530"/>
<path d="M6 2.5H12.5C13.0523 2.5 13.5 2.94772 13.5 3.5V10" stroke="#1F2530"/>
</svg>
"##;
        let mut f = std::fs::File::create(&svg_path).unwrap();
        f.write_all(svg.as_bytes()).unwrap();
        drop(f);

        let icon_path =
            try_convert_svg_to_chromium_icon(svg_path.to_str().unwrap(), "icon-restore.icon")
                .expect("svg -> icon should succeed");
        let icon_text = std::fs::read_to_string(&icon_path).unwrap();
        assert!(
            icon_text.contains("STROKE, 1,\r\n"),
            "back window L-stroke must use STROKE:\n{}",
            icon_text
        );
        assert!(
            icon_text.contains("CUBIC_TO,"),
            "rounded corner cubic must be preserved:\n{}",
            icon_text
        );
        assert!(
            !icon_text.contains("CLOSE,"),
            "open L-stroke must not be closed (would fill a triangle):\n{}",
            icon_text
        );

        let svg_back = try_convert_chromium_icon_path_to_svg_markup(&icon_path)
            .expect("icon -> svg should succeed");
        assert!(
            svg_back.contains("stroke=") && svg_back.contains("fill=\"none\""),
            "reverse preview should render stroked open path:\n{}",
            svg_back
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 嵌入式 PNG 头像（pattern + <image>）不能转为矢量 .icon，应明确报错而非黑圆占位。
    #[test]
    fn svg_embedded_png_avatar_is_rejected() {
        let dir = std::env::temp_dir().join(format!("avart_reject_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let svg_path = dir.join("avatar.svg");
        let svg = br##"<svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
<rect width="16" height="16" rx="8" fill="url(#pattern0)"/>
<defs>
<pattern id="pattern0" patternContentUnits="objectBoundingBox" width="1" height="1">
<use href="#image0"/>
</pattern>
<image id="image0" width="4" height="4" href="data:image/png;base64,AAAA"/>
</defs>
</svg>"##;
        std::fs::write(&svg_path, svg).unwrap();
        let err =
            try_convert_svg_to_chromium_icon(svg_path.to_str().unwrap(), "avatar.icon").unwrap_err();
        assert!(
            err.contains("位图") || err.contains("PNG"),
            "expected raster rejection, got: {}",
            err
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Figma 内描边 mask：`STROKE` 减半 + 路径内缩至半像素（3,3,10,10 → 3.5,3.5,9,9），
    /// 对齐 `browser_window_maximum.icon`。
    #[test]
    fn figma_inside_stroke_mask_halves_stroke_width() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!(
            "chromium_icon_inside_mask_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let svg_path = dir.join("icon-max.svg");

        let svg = r##"<svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
<mask id="path-1-inside-1_6247_21705" fill="white">
<rect x="3" y="3" width="10" height="10" rx="0.875"/>
</mask>
<rect x="3" y="3" width="10" height="10" rx="0.875" stroke="#1F2530" stroke-width="2" mask="url(#path-1-inside-1_6247_21705)"/>
</svg>
"##;
        let mut f = std::fs::File::create(&svg_path).unwrap();
        f.write_all(svg.as_bytes()).unwrap();
        drop(f);

        let icon_path =
            try_convert_svg_to_chromium_icon(svg_path.to_str().unwrap(), "icon-max.icon")
                .expect("svg -> icon should succeed");
        let icon_text = std::fs::read_to_string(&icon_path).unwrap();
        assert!(
            icon_text.contains("STROKE, 1,\r\n"),
            "inside-stroke mask must halve stroke-width 2 -> 1, got:\n{}",
            icon_text
        );
        assert!(
            !icon_text.contains("STROKE, 2,"),
            "must not keep full stroke-width 2 without mask:\n{}",
            icon_text
        );
        assert!(
            icon_text.contains("ROUND_RECT, 3.5, 3.5, 9, 9, 0.88,"),
            "inside-stroke must inset geometry to match Chromium browser_window_maximum:\n{}",
            icon_text
        );
        assert!(
            !icon_text.contains("ROUND_RECT, 3, 3, 10, 10,"),
            "must not use un-inset SVG rect geometry:\n{}",
            icon_text
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `<defs>/<clipPath>/<rect fill="white"/>` 仅供裁剪定义，不能当作可见几何输出；
    /// 否则会在 .icon 末尾多一层整幅白色 ROUND_RECT，预览表现为白屏（如 ic_about_browser_16）。
    #[test]
    fn svg_defs_clip_path_white_rect_must_not_emit_round_rect() {
        let dir = std::env::temp_dir().join(format!(
            "chromium_icon_defs_clip_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let svg_path = dir.join("clip.svg");
        let svg = br#"<?xml version="1.0" encoding="UTF-8"?>
<svg width="16" height="16" viewBox="0 0 16 16" xmlns="http://www.w3.org/2000/svg">
<g clip-path="url(#c)">
<path d="M8 0L16 8L8 16Z" fill="black"/>
</g>
<defs>
<clipPath id="c"><rect width="16" height="16" fill="white"/></clipPath>
</defs>
</svg>"#;
        std::fs::write(&svg_path, svg).unwrap();
        let icon_path =
            try_convert_svg_to_chromium_icon(svg_path.to_str().unwrap(), "clip.icon").unwrap();
        let txt = std::fs::read_to_string(&icon_path).unwrap();
        assert!(
            !txt.contains("ROUND_RECT"),
            "clipPath helper rect must not become ROUND_RECT:\n{}",
            txt
        );
        assert!(
            !txt.contains("PATH_COLOR_ARGB, 0xFF, 0xFF, 0xFF, 0xFF,"),
            "clipPath white must not emit drawable white layer:\n{}",
            txt
        );
        assert!(
            txt.contains("PATH_COLOR_ARGB, 0xFF, 0x00, 0x00, 0x00,"),
            "visible path fill (black) missing:\n{}",
            txt
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `<clipPath>` 若在 `<defs>` 外仍会包含整幅白色底 rect；必须整块跳过，
    /// 不能仅靠 `<defs>` 深度判断。
    #[test]
    fn svg_clip_path_outside_defs_white_helper_not_emit_round_rect() {
        let dir = std::env::temp_dir().join(format!(
            "chromium_icon_clip_outside_defs_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let svg_path = dir.join("clip_outside.svg");
        let svg = br#"<?xml version="1.0" encoding="UTF-8"?>
<svg width="16" height="16" viewBox="0 0 16 16" xmlns="http://www.w3.org/2000/svg">
<clipPath id="c"><rect width="16" height="16" fill="white"/></clipPath>
<path d="M8 0L16 8L8 16Z" fill="black"/>
</svg>"#;
        std::fs::write(&svg_path, svg).unwrap();
        let icon_path =
            try_convert_svg_to_chromium_icon(svg_path.to_str().unwrap(), "clip_out.icon").unwrap();
        let txt = std::fs::read_to_string(&icon_path).unwrap();
        assert!(
            !txt.contains("ROUND_RECT"),
            "clipPath helper rect must not become ROUND_RECT:\n{}",
            txt
        );
        assert!(
            !txt.contains("PATH_COLOR_ARGB, 0xFF, 0xFF, 0xFF, 0xFF,"),
            "clipPath white must not emit drawable white layer:\n{}",
            txt
        );
        assert!(
            txt.contains("PATH_COLOR_ARGB, 0xFF, 0x00, 0x00, 0x00,"),
            "visible path missing:\n{}",
            txt
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 根 `<svg fill="none">` 下，闭合 `<path>` 无 `fill`、只有 `stroke` 时须写出 PATH_COLOR，
    /// 否则反向预览会整块发白。开放折线不能把 `stroke` 伪造成 `fill`（会变成错误实心块）。
    #[test]
    fn svg_fill_none_root_stroke_fallback_writes_path_color() {
        let dir = std::env::temp_dir().join(format!(
            "chromium_icon_stroke_fallback_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let svg_path = dir.join("stroke_fb.svg");
        let svg = br#"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none">
<path d="M1 1 H5 V5 H1 Z" stroke="rgb(17,34,51)" stroke-width="2"/>
</svg>"#;
        std::fs::write(&svg_path, svg).unwrap();
        let icon_path =
            try_convert_svg_to_chromium_icon(svg_path.to_str().unwrap(), "stroke_fb.icon")
                .unwrap();
        let txt = std::fs::read_to_string(&icon_path).unwrap();
        assert!(
            txt.contains("PATH_COLOR_ARGB, 0xFF, 0x11, 0x22, 0x33,"),
            "expected stroke color copied to PATH_COLOR:\n{}",
            txt
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn svg_emit_path_colors_false_skips_path_color_argb_lines() {
        let dir = std::env::temp_dir().join(format!(
            "chromium_icon_no_paint_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let svg_path = dir.join("red_sq.svg");
        let svg = br##"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8" viewBox="0 0 8 8">
<path fill="#ff0000" d="M0 0 H8 V8 H0 Z"/>
</svg>"##;
        std::fs::write(&svg_path, svg).unwrap();
        let opts = SvgToChromiumIconOptions {
            emit_path_colors: false,
        };
        let icon_path = try_convert_svg_to_chromium_icon_with_options(
            svg_path.to_str().unwrap(),
            "nocolor.icon",
            &opts,
        )
        .unwrap();
        let txt = std::fs::read_to_string(&icon_path).unwrap();
        assert!(
            !txt.contains("PATH_COLOR_ARGB"),
            "expected no PATH_COLOR when emit_path_colors=false:\n{}",
            txt
        );
        assert!(
            txt.contains("CANVAS_DIMENSIONS") && txt.contains("CLOSE,"),
            "geometry should still emit:\n{}",
            txt
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 仅描边的 `<circle>` 不能写成实心 `CIRCLE`（放大镜等会变成整块黑饼）。
    #[test]
    fn stroke_only_circle_becomes_evenodd_ring_not_solid_circle() {
        let dir = std::env::temp_dir().join(format!(
            "chromium_icon_stroke_circ_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let svg_path = dir.join("zoom_like.svg");
        let svg = br##"<?xml version="1.0"?>
<svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
<circle cx="7.5" cy="7.5" r="6" stroke="#222222"/>
<path d="M11.5 11.5L15 15" stroke="#222222"/>
</svg>"##;
        std::fs::write(&svg_path, svg).unwrap();
        let icon_path =
            try_convert_svg_to_chromium_icon(svg_path.to_str().unwrap(), "zoom_like.icon").unwrap();
        let txt = std::fs::read_to_string(&icon_path).unwrap();
        assert!(
            txt.contains("PATH_COLOR_ARGB, 0xFF, 0x22, 0x22, 0x22,"),
            "ring stroke color missing:\n{}",
            txt
        );
        assert!(
            txt.matches("CLOSE,").count() >= 3,
            "donut (2x CLOSE in ring path) + STROKE handle (1x CLOSE) expected, got:\n{}",
            txt
        );
        assert!(
            txt.contains("STROKE,"),
            "open diagonal handle must use native STROKE:\n{}",
            txt
        );
        assert!(
            !txt.contains("\nCIRCLE,"),
            "stroke-only lens must not be a solid CIRCLE line:\n{}",
            txt
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 仅 stroke 的开放折线在 Chromium `.icon` 中应写成 `STROKE` + 折线段 + `CLOSE`，与普通 Material 矢量图标管线一致。
    #[test]
    fn svg_open_stroked_diagonal_emits_native_stroke_command() {
        let dir = std::env::temp_dir().join(format!(
            "chromium_icon_open_stroke_line_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let svg_path = dir.join("slash.svg");
        let svg = br##"<?xml version="1.0"?>
<svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
<path d="M1 1 L10 10" stroke="#112233" stroke-width="2"/>
</svg>"##;
        std::fs::write(&svg_path, svg).unwrap();
        let icon_path =
            try_convert_svg_to_chromium_icon(svg_path.to_str().unwrap(), "slash.icon").unwrap();
        let txt = std::fs::read_to_string(&icon_path).unwrap();
        assert!(
            txt.contains("PATH_COLOR_ARGB, 0xFF, 0x11, 0x22, 0x33,"),
            "stroke color should appear as fill/PATH_COLOR:\n{}",
            txt
        );
        assert!(
            txt.contains("STROKE, 2,\r\n"),
            "expected STROKE with width 2, got:\n{}",
            txt
        );
        assert!(
            txt.contains("MOVE_TO, 1, 1,\r\n")
                && txt.contains("LINE_TO, 10, 10,\r\n")
                && txt.contains("CLOSE,"),
            "expected MOVE/LINE/CLOSE polyline compatible with Chromium paint path:\n{}",
            txt
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `viewBox="… 464.955 464.955"` 这类非整数画布在旧版本会写成
    /// `CANVAS_DIMENSIONS, 464.95,`，Chromium 端按整数解析时会失败。
    /// 现在必须四舍五入到整数。
    #[test]
    fn canvas_dimensions_rounded_to_integer() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!(
            "chromium_icon_canvas_test_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let svg_path = dir.join("non_integer_viewbox.svg");
        let icon_name = "non_integer_viewbox.icon";

        let svg = r##"<?xml version="1.0"?>
<svg width="800px" height="800px" viewBox="-33.71 0 464.955 464.955" xmlns="http://www.w3.org/2000/svg">
  <path fill="#ff0000" d="M0 0 L10 10 Z"/>
</svg>
"##;
        let mut f = std::fs::File::create(&svg_path).unwrap();
        f.write_all(svg.as_bytes()).unwrap();
        drop(f);

        let icon_path = try_convert_svg_to_chromium_icon(svg_path.to_str().unwrap(), icon_name)
            .expect("svg -> icon should succeed");
        let icon_text = std::fs::read_to_string(&icon_path).unwrap();
        assert!(
            icon_text.contains("CANVAS_DIMENSIONS, 465,"),
            "expected rounded integer 465, got: {}",
            icon_text.lines().take(8).collect::<Vec<_>>().join("\\n")
        );
        assert!(
            !icon_text.contains("CANVAS_DIMENSIONS, 464.95"),
            "fractional CANVAS_DIMENSIONS leaked through: {}",
            icon_text
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 历史 .icon 里残留的 `CANVAS_DIMENSIONS, 464.95,` 在反向解析时也要按整数对齐，
    /// 否则生成的 SVG `viewBox`/`width`/`height` 会跟 Chromium 期望不符。
    #[test]
    fn reverse_canvas_dimensions_rounds_legacy_fraction() {
        let icon = "CANVAS_DIMENSIONS, 464.95,\nMOVE_TO, 0, 0,\nLINE_TO, 1, 1,\nCLOSE,\n";
        let svg = try_convert_chromium_icon_source_to_svg_markup(icon).expect("should convert");
        assert!(
            svg.contains("viewBox=\"0 0 465 465\""),
            "expected rounded viewBox 0 0 465 465, got: {}",
            svg
        );
    }

    #[test]
    fn reverse_stroke_polyline_maps_to_svg_stroke_attributes() {
        let icon = "CANVAS_DIMENSIONS, 16,\r\nPATH_COLOR_ARGB, 0xFF, 0x11, 0x22, 0x33,\r\nFILL_RULE_NONZERO,\r\nSTROKE, 2,\r\nMOVE_TO, 1, 1,\r\nLINE_TO, 10, 10,\r\nCLOSE,\r\n";
        let svg =
            try_convert_chromium_icon_source_to_svg_markup(icon).expect("should convert");
        assert!(
            svg.contains("stroke=\"#112233\""),
            "expected stroke hex from PATH_COLOR on stroke layer, got: {}",
            svg
        );
        assert!(
            svg.contains("stroke-width=\"2\"") || svg.contains("stroke-width='2'"),
            "expected stroke-width 2, got: {}",
            svg
        );
        assert!(
            svg.contains("fill=\"none\""),
            "stroked vector path must not use fill-only paint in preview: {}",
            svg
        );
    }

    #[test]
    fn inline_style_overrides_class_and_attribute() {
        let mut sheet: std::collections::HashMap<String, std::collections::HashMap<String, String>> =
            std::collections::HashMap::new();
        sheet.insert(
            ".x".to_string(),
            [("fill".to_string(), "#abcdef".to_string())].into_iter().collect(),
        );
        let mut attrs: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
        attrs.insert("class".to_string(), Value::from("x"));
        attrs.insert("fill".to_string(), Value::from("#000000"));
        attrs.insert("style".to_string(), Value::from("fill: #ff0000"));

        let resolved = resolve_svg_styles(&sheet, &attrs, "path");
        assert_eq!(resolved.get("fill").unwrap().to_string(), "#ff0000");
    }
}
