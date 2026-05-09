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
fn format_number(num: f32) -> String {
    if num.is_nan() {
        return "0".to_string();
    }
    // 整数直接以整数形式输出（避免 `1.0` 这种冗余小数位）。
    if num.fract() == 0.0 && num.abs() < 1.0e9 {
        return format!("{}", num as i64);
    }

    // 保留 2 位小数（向零截断），并去掉末尾多余的 0 与可能残留的小数点。
    // 注意正负数都使用一致的截断策略，避免出现非对称的舍入。
    let truncated = (num * 100.0).trunc() / 100.0;
    let mut s = format!("{:.2}", truncated);
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

/// 把 SVG `<rect>` 转换为 Chromium 命令。
///
/// Chromium 的 `ROUND_RECT` 接受 `(x, y, w, h, r)` —— **只有一个圆角半径**。
/// 因此当 `rx != ry` 时无法用单条命令精确表示，这里取 `min(rx, ry)` 折中。
/// 当 `rx == 0 && ry == 0` 时退化为普通矩形（仍可用 `ROUND_RECT` 半径 0 表示）。
///
/// 调用方负责在调用前决定是否需要 `NEW_PATH`。
fn handle_svg_rect(
    _tag_type: &Type,
    attributes: &std::collections::HashMap<String, Value>,
    write_new_path: bool,
) -> String {
    let mut output = String::new();

    let x = parse_attr_f32(attributes, "x", 0.0);
    let y = parse_attr_f32(attributes, "y", 0.0);
    let width = parse_attr_f32(attributes, "width", 0.0);
    let height = parse_attr_f32(attributes, "height", 0.0);

    // SVG 规则：rx / ry 互相回退（只指定一个时另一个等于它），均未指定时为 0。
    let rx_attr = attributes.get("rx").and_then(parse_dim).map(|d| d as f32);
    let ry_attr = attributes.get("ry").and_then(parse_dim).map(|d| d as f32);
    let (rx, _ry) = match (rx_attr, ry_attr) {
        (Some(a), Some(b)) => (a.min(b), a.min(b)),
        (Some(a), None) => (a, a),
        (None, Some(b)) => (b, b),
        (None, None) => (0.0, 0.0),
    };

    if write_new_path {
        output.push_str("NEW_PATH,\r\n");
    }
    if let Some(fill) = attributes.get("fill") {
        let color = color_to_argb(&fill.to_string());
        if !color.is_empty() {
            output.push_str(&format!("PATH_COLOR_ARGB, {},\r\n", color));
        }
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

/// 把 SVG `<circle>` 转换为 Chromium `CIRCLE, cx, cy, r,`。
fn handle_svg_circle(
    _tag_type: &Type,
    attributes: &std::collections::HashMap<String, Value>,
    write_new_path: bool,
) -> String {
    let mut output = String::new();

    let cx = parse_attr_f32(attributes, "cx", 0.0);
    let cy = parse_attr_f32(attributes, "cy", 0.0);
    let r = parse_attr_f32(attributes, "r", 0.0);

    if write_new_path {
        output.push_str("NEW_PATH,\r\n");
    }
    if let Some(fill) = attributes.get("fill") {
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

/// 把 SVG `<ellipse>` 转换为 Chromium `OVAL, cx, cy, rx, ry,`。
fn handle_svg_ellipse(
    _tag_type: &Type,
    attributes: &std::collections::HashMap<String, Value>,
    write_new_path: bool,
) -> String {
    let mut output = String::new();

    let cx = parse_attr_f32(attributes, "cx", 0.0);
    let cy = parse_attr_f32(attributes, "cy", 0.0);
    let rx = parse_attr_f32(attributes, "rx", 0.0);
    let ry = parse_attr_f32(attributes, "ry", 0.0);

    if write_new_path {
        output.push_str("NEW_PATH,\r\n");
    }
    if let Some(fill) = attributes.get("fill") {
        let color = color_to_argb(&fill.to_string());
        if !color.is_empty() {
            output.push_str(&format!("PATH_COLOR_ARGB, {},\r\n", color));
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

/// 把 SVG `<path>` 的 `d` 属性翻译为一段 Chromium 路径命令。
fn handle_svg_path(
    attributes: &std::collections::HashMap<String, Value>,
    write_new_path: bool,
) -> String {
    let mut output = String::new();

    if write_new_path {
        output.push_str("NEW_PATH,\r\n");
    }

    if let Some(fill) = attributes.get("fill") {
        let color = color_to_argb(&fill.to_string());
        if !color.is_empty() {
            output.push_str(&format!("PATH_COLOR_ARGB, {},\r\n", color));
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

    let mut pen = PenState::default();
    let mut last_cubic_ctrl: Option<(f32, f32)> = None;

    for command in parsed.iter() {
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
                // Chromium 命令名是 *_SHORTHAND，不是 SMOOTH_*。
                // 同时 R_QUADRATIC_TO_SHORTHAND 是存在的。
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
                            // 记录“绝对坐标系下”的最后控制点，方便后续 S/s 反射。
                            last_cubic_ctrl = Some((pen.cur_x + x2, pen.cur_y + y2));
                            pen.line_rel(x, y);
                        }
                    }
                }
            }
            Command::SmoothCubicCurve(position, params) => {
                // Chromium 只有 CUBIC_TO_SHORTHAND（绝对版），没有相对版本，
                // 所以 `s` 必须就地展开为绝对坐标的 CUBIC_TO_SHORTHAND。
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
                    // 标志位必须是整数 0/1。
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

/// 出错友好版的 SVG -> .icon 转换。错误会以可阅读的字符串返回，而不是 panic。
pub fn try_convert_svg_to_chromium_icon(
    svg_path: &str,
    output_path: &str,
) -> Result<String, String> {
    let mut content = String::new();
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

    let events = svg::open(svg_path, &mut content)
        .map_err(|e| format!("Failed to open/parse SVG '{}': {}", svg_path, e))?
        .collect::<Vec<_>>();

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

    let mut emitted_path = false;
    for event in events.iter() {
        match event {
            Event::Tag("g", t, attributes) if is_open_tag(t) => {
                if attributes.contains_key("transform") {
                    eprintln!(
                        "[chromium_icon] warning: <g transform=...> is not supported, \
                         the transform will be ignored. Please flatten transforms in your SVG first."
                    );
                }
            }
            Event::Tag("path", t, attributes) if is_open_tag(t) => {
                let resolved = resolve_svg_styles(&stylesheet, attributes, "path");
                let data = handle_svg_path(&resolved, emitted_path);
                if !data.is_empty() {
                    write!(output_file, "{}", data)
                        .map_err(|e| format!("Failed to write path: {}", e))?;
                    emitted_path = true;
                }
            }
            Event::Tag("circle", t, attributes) if is_open_tag(t) => {
                let resolved = resolve_svg_styles(&stylesheet, attributes, "circle");
                let data = handle_svg_circle(t, &resolved, emitted_path);
                if !data.is_empty() {
                    write!(output_file, "{}", data)
                        .map_err(|e| format!("Failed to write circle: {}", e))?;
                    emitted_path = true;
                }
            }
            Event::Tag("rect", t, attributes) if is_open_tag(t) => {
                let resolved = resolve_svg_styles(&stylesheet, attributes, "rect");
                let data = handle_svg_rect(t, &resolved, emitted_path);
                if !data.is_empty() {
                    write!(output_file, "{}", data)
                        .map_err(|e| format!("Failed to write rect: {}", e))?;
                    emitted_path = true;
                }
            }
            Event::Tag("ellipse", t, attributes) if is_open_tag(t) => {
                let resolved = resolve_svg_styles(&stylesheet, attributes, "ellipse");
                let data = handle_svg_ellipse(t, &resolved, emitted_path);
                if !data.is_empty() {
                    write!(output_file, "{}", data)
                        .map_err(|e| format!("Failed to write ellipse: {}", e))?;
                    emitted_path = true;
                }
            }
            _ => {}
        }
    }

    Ok(dst.to_string_lossy().into_owned())
}

/// 反向转换时的一层 SVG 子节点（路径或 Chromium 基本形）。
enum ReverseIconLayer {
    Path {
        fill_rule: String,
        fill: Option<String>,
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
    },
}

/// Chromium 未写 `PATH_COLOR_ARGB` 的路径在运行时由 `CreateVectorIcon(..., color)` 上色；
/// 导出为静态 SVG 时没有该颜色，若用 SVG 默认黑色会与设计稿（常见为白底/反白）不符。
/// 这里用白色作为模板区默认填充；`evenodd` 镂空则依赖下方同色的全画布衬底显出「白心」。
const REVERSE_ICON_TEMPLATE_FILL: &str = "#ffffff";

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
    out: &mut Vec<ReverseIconLayer>,
) {
    if !path_nonempty {
        return;
    }
    out.push(ReverseIconLayer::Path {
        fill_rule: fill_rule.to_string(),
        fill: fill.clone(),
        data,
    });
}

/// 把 Chromium `.icon` 源文本反向解析为 SVG 字符串（供 `<img src>` 等预览）。
///
/// 支持 `NEW_PATH` 多子路径、`CIRCLE` / `OVAL` / `ROUND_RECT` 与路径命令混合，
/// 与正向 `handle_svg_*` 输出对齐，避免信封等形状在预览中丢失。
pub fn try_convert_chromium_icon_source_to_svg_markup(source: &str) -> Result<String, String> {
    let mut layers: Vec<ReverseIconLayer> = Vec::new();
    let mut path_data = Data::new();
    let mut path_nonempty = false;
    let mut canvas_dimensions: u32 = 24;
    let mut fill_rule = "evenodd".to_string();
    let mut fill_color: Option<String> = None;
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
                let old = std::mem::replace(&mut path_data, Data::new());
                let had = path_nonempty;
                path_nonempty = false;
                flush_icon_path_layer(old, had, &fill_rule, &fill_color, &mut layers);
                pen = PenState::default();
            }
            "CIRCLE" => {
                let old = std::mem::replace(&mut path_data, Data::new());
                let had = path_nonempty;
                path_nonempty = false;
                flush_icon_path_layer(old, had, &fill_rule, &fill_color, &mut layers);
                pen = PenState::default();
                layers.push(ReverseIconLayer::Circle {
                    cx: pf(1),
                    cy: pf(2),
                    r: pf(3),
                    fill: fill_color.clone(),
                });
            }
            "OVAL" => {
                let old = std::mem::replace(&mut path_data, Data::new());
                let had = path_nonempty;
                path_nonempty = false;
                flush_icon_path_layer(old, had, &fill_rule, &fill_color, &mut layers);
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
                let old = std::mem::replace(&mut path_data, Data::new());
                let had = path_nonempty;
                path_nonempty = false;
                flush_icon_path_layer(old, had, &fill_rule, &fill_color, &mut layers);
                pen = PenState::default();
                layers.push(ReverseIconLayer::RoundRect {
                    x: pf(1),
                    y: pf(2),
                    w: pf(3),
                    h: pf(4),
                    r: pf(5),
                    fill: fill_color.clone(),
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
            "PATH_COLOR_ALPHA" | "PATH_MODE_CLEAR" | "STROKE" | "CAP_SQUARE" | "CLIP"
            | "DISABLE_AA" | "FLIPS_IN_RTL" => {
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
            .set("fill", REVERSE_ICON_TEMPLATE_FILL),
    );

    for layer in layers {
        doc = match layer {
            ReverseIconLayer::Path {
                fill_rule,
                fill,
                data,
            } => {
                let mut p = SvgPath::new()
                    .set("fill-rule", fill_rule.as_str())
                    .set("d", data);
                p = if let Some(ref c) = fill {
                    p.set("fill", c.as_str())
                } else {
                    p.set("fill", REVERSE_ICON_TEMPLATE_FILL)
                };
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
                    c.set("fill", REVERSE_ICON_TEMPLATE_FILL)
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
                    e.set("fill", REVERSE_ICON_TEMPLATE_FILL)
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
            } => {
                let mut rect = Rectangle::new()
                    .set("x", x)
                    .set("y", y)
                    .set("width", w)
                    .set("height", h);
                if r > 0.0 {
                    rect = rect.set("rx", r).set("ry", r);
                }
                rect = if let Some(ref f) = fill {
                    rect.set("fill", f.as_str())
                } else {
                    rect.set("fill", REVERSE_ICON_TEMPLATE_FILL)
                };
                doc.add(rect)
            }
        };
    }

    Ok(doc.to_string())
}

/// 从磁盘上的 `.icon` 文件生成 SVG 字符串（浏览器 `<img src="...svg">` 预览用）。
pub fn try_convert_chromium_icon_path_to_svg_markup(icon_path: &str) -> Result<String, String> {
    let source =
        std::fs::read_to_string(icon_path).map_err(|e| format!("Failed to read icon file: {}", e))?;
    try_convert_chromium_icon_source_to_svg_markup(&source)
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
        assert_eq!(format_number(-0.97), "-0.97");
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
