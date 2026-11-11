//! Chromium command stream decoding. A NEW_PATH, not a primitive command,
//! determines the boundary of a draw call. The default representation is the
//! last CANVAS_DIMENSIONS block, matching GetDefaultSizeOfVectorIcon().

use std::fmt::Write;

#[derive(Debug)]
struct Command {
    name: String,
    args: Vec<f64>,
}

fn argument_count(name: &str) -> Option<usize> {
    Some(match name {
        "NEW_PATH" | "FILL_RULE_NONZERO" | "PATH_MODE_CLEAR" | "CAP_SQUARE" | "CLOSE"
        | "DISABLE_AA" | "FLIPS_IN_RTL" => 0,
        "PATH_COLOR_ALPHA" | "STROKE" | "H_LINE_TO" | "R_H_LINE_TO" | "V_LINE_TO"
        | "R_V_LINE_TO" | "CANVAS_DIMENSIONS" => 1,
        "MOVE_TO"
        | "R_MOVE_TO"
        | "LINE_TO"
        | "R_LINE_TO"
        | "QUADRATIC_TO_SHORTHAND"
        | "R_QUADRATIC_TO_SHORTHAND" => 2,
        "CIRCLE" => 3,
        "PATH_COLOR_ARGB" | "CUBIC_TO_SHORTHAND" | "QUADRATIC_TO" | "R_QUADRATIC_TO" | "OVAL"
        | "CLIP" => 4,
        "ROUND_RECT" => 5,
        "CUBIC_TO" | "R_CUBIC_TO" => 6,
        "ARC_TO" | "R_ARC_TO" => 7,
        _ => return None,
    })
}

fn parse_number(token: &str) -> Result<f64, String> {
    // A trailing hex digit F is not a C++ floating-point suffix.
    let value = if let Some(hex) = token
        .strip_prefix("0x")
        .or_else(|| token.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16)
            .map(|n| n as f64)
            .map_err(|_| format!("Invalid hexadecimal .icon argument: {token}"))?
    } else {
        token
            .strip_suffix(['f', 'F'])
            .unwrap_or(token)
            .parse::<f64>()
            .map_err(|_| format!("Invalid .icon numeric argument: {token}"))?
    };
    if !value.is_finite() || value.abs() > f32::MAX as f64 {
        return Err(format!(
            "Non-finite or out-of-range .icon argument: {token}"
        ));
    }
    // PathElement stores a SkScalar (float32). Quantize before operations such
    // as alpha flooring and canvas truncation to match Chromium's input values.
    Ok(f64::from(value as f32))
}

fn parse_commands(source: &str) -> Result<Vec<Command>, String> {
    let mut clean = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '/' && chars.peek() == Some(&'/') {
            chars.next();
            for ch in chars.by_ref() {
                if ch == '\n' {
                    break;
                }
            }
            clean.push(' ');
        } else if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut ended = false;
            while let Some(ch) = chars.next() {
                if ch == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    ended = true;
                    break;
                }
            }
            if !ended {
                return Err("Unterminated .icon comment".into());
            }
            clean.push(' ');
        } else {
            clean.push(ch);
        }
    }
    let mut tokens = clean
        .trim_start_matches('\u{feff}')
        .split(|c: char| c == ',' || c.is_ascii_whitespace())
        .filter(|s| !s.is_empty());
    let mut commands = Vec::new();
    while let Some(name) = tokens.next() {
        let count = argument_count(name)
            .ok_or_else(|| format!("Unknown .icon command or unexpected argument: {name}"))?;
        let mut args = Vec::with_capacity(count);
        for index in 0..count {
            let token = tokens.next().ok_or_else(|| {
                format!(
                    "{name} requires {count} arguments; missing argument {}",
                    index + 1
                )
            })?;
            args.push(parse_number(token).map_err(|error| format!("{name}: {error}"))?);
        }
        commands.push(Command {
            name: name.to_string(),
            args,
        });
    }
    if commands.is_empty() {
        return Err("The .icon command stream is empty".into());
    }
    Ok(commands)
}

fn number(value: f64) -> String {
    if value == 0.0 {
        "0".into()
    } else {
        value.to_string()
    }
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
}

#[derive(Clone, Copy, Default)]
struct Point {
    x: f64,
    y: f64,
}

impl Point {
    fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
    fn offset(self, x: f64, y: f64) -> Self {
        Self::new(self.x + x, self.y + y)
    }
}

struct Path {
    data: String,
    current: Point,
    // SkPathBuilder::getLastPt() retains the last explicit point after CLOSE,
    // while relative path operations restart at the contour's initial point.
    last_point: Point,
    start: Point,
    has_current: bool,
    last_control: Point,
    previous_is_curve: bool,
    color: [u8; 4], // RGBA; template color chosen for the default LTR preview.
    nonzero: bool,
    stroke: Option<f64>,
    square_cap: bool,
    clear: bool,
    disable_aa: bool,
}

impl Default for Path {
    fn default() -> Self {
        Self {
            data: String::new(),
            current: Point::default(),
            last_point: Point::default(),
            start: Point::default(),
            has_current: false,
            last_control: Point::default(),
            previous_is_curve: false,
            color: [0x42, 0x42, 0x42, 255],
            nonzero: false,
            stroke: None,
            square_cap: false,
            clear: false,
            disable_aa: false,
        }
    }
}

impl Path {
    fn with_color(color: [u8; 4]) -> Self {
        Self {
            color,
            ..Self::default()
        }
    }

    fn move_to(&mut self, p: Point) {
        write!(self.data, "M{} {} ", number(p.x), number(p.y)).unwrap();
        self.current = p;
        self.last_point = p;
        self.start = p;
        self.has_current = true;
    }

    fn ensure_move(&mut self) {
        if !self.has_current {
            self.move_to(Point::default());
        }
    }

    fn line_to(&mut self, p: Point) {
        self.ensure_move();
        write!(self.data, "L{} {} ", number(p.x), number(p.y)).unwrap();
        self.current = p;
        self.last_point = p;
    }

    fn close(&mut self) {
        if self.has_current {
            self.data.push_str("Z ");
            self.current = self.start;
        }
    }

    fn cubic_to(&mut self, first: Point, second: Point, end: Point) {
        self.ensure_move();
        write!(
            self.data,
            "C{} {} {} {} {} {} ",
            number(first.x),
            number(first.y),
            number(second.x),
            number(second.y),
            number(end.x),
            number(end.y)
        )
        .unwrap();
        self.current = end;
        self.last_point = end;
        self.last_control = second;
    }

    fn quadratic_to(&mut self, control: Point, end: Point) {
        self.ensure_move();
        write!(
            self.data,
            "Q{} {} {} {} ",
            number(control.x),
            number(control.y),
            number(end.x),
            number(end.y)
        )
        .unwrap();
        self.current = end;
        self.last_point = end;
        self.last_control = control;
    }

    fn arc_to(&mut self, rx: f64, ry: f64, angle: f64, large: bool, sweep: bool, end: Point) {
        self.ensure_move();
        if rx == 0.0 || ry == 0.0 || (end.x == self.current.x && end.y == self.current.y) {
            self.line_to(end);
        } else {
            write!(
                self.data,
                "A{} {} {} {} {} {} {} ",
                number(rx.abs()),
                number(ry.abs()),
                number(angle),
                u8::from(large),
                u8::from(sweep),
                number(end.x),
                number(end.y)
            )
            .unwrap();
            self.current = end;
            self.last_point = end;
        }
    }

    fn oval(&mut self, cx: f64, cy: f64, rx: f64, ry: f64) -> Result<(), String> {
        if rx < 0.0 || ry < 0.0 {
            return Err("CIRCLE/OVAL radii must be nonnegative".into());
        }
        self.move_to(Point::new(cx + rx, cy));
        self.arc_to(rx, ry, 0.0, false, true, Point::new(cx, cy + ry));
        self.arc_to(rx, ry, 0.0, false, true, Point::new(cx - rx, cy));
        self.arc_to(rx, ry, 0.0, false, true, Point::new(cx, cy - ry));
        self.arc_to(rx, ry, 0.0, false, true, Point::new(cx + rx, cy));
        self.close();
        Ok(())
    }

    fn round_rect(
        &mut self,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        radius: f64,
    ) -> Result<(), String> {
        if width <= 0.0 || height <= 0.0 || radius < 0.0 {
            return Err(
                "ROUND_RECT dimensions must be positive and radius must be nonnegative".into(),
            );
        }
        // SkRRect scales both radii by the same factor to fit its bounds.
        let r = radius.min(width / 2.0).min(height / 2.0);
        if r == 0.0 {
            // Skia reduces this to a rectangle starting at bottom-left. The
            // closing edge is implicit, so getLastPt() remains bottom-right.
            self.move_to(Point::new(x, y + height));
            self.line_to(Point::new(x, y));
            self.line_to(Point::new(x + width, y));
            self.line_to(Point::new(x + width, y + height));
            self.close();
            return Ok(());
        }
        self.move_to(Point::new(x, y + height - r)); // Skia's clockwise start index 6.
        self.line_to(Point::new(x, y + r));
        self.arc_to(r, r, 0.0, false, true, Point::new(x + r, y));
        self.line_to(Point::new(x + width - r, y));
        self.arc_to(r, r, 0.0, false, true, Point::new(x + width, y + r));
        self.line_to(Point::new(x + width, y + height - r));
        self.arc_to(
            r,
            r,
            0.0,
            false,
            true,
            Point::new(x + width - r, y + height),
        );
        self.line_to(Point::new(x + r, y + height));
        self.arc_to(r, r, 0.0, false, true, Point::new(x, y + height - r));
        self.close();
        Ok(())
    }

    fn markup(&self, mask: bool) -> String {
        let color = if mask {
            "#000000".to_string()
        } else {
            format!(
                "#{:02X}{:02X}{:02X}",
                self.color[0], self.color[1], self.color[2]
            )
        };
        let opacity = if mask {
            1.0
        } else {
            self.color[3] as f64 / 255.0
        };
        let rule = if self.nonzero { "nonzero" } else { "evenodd" };
        let mut result = format!("<path d=\"{}\" fill-rule=\"{}\"", self.data.trim(), rule);
        if let Some(width) = self.stroke {
            write!(result, " fill=\"none\" stroke=\"{}\" stroke-width=\"{}\" stroke-linecap=\"{}\" stroke-linejoin=\"miter\" stroke-miterlimit=\"4\" stroke-opacity=\"{}\"",
                color, number(width), if self.square_cap { "square" } else { "round" }, number(opacity)).unwrap();
        } else {
            write!(
                result,
                " fill=\"{}\" fill-opacity=\"{}\"",
                color,
                number(opacity)
            )
            .unwrap();
        }
        if self.disable_aa {
            result.push_str(" shape-rendering=\"crispEdges\"");
        }
        result.push_str("/>");
        result
    }
}

fn channel(value: f64) -> Result<u8, String> {
    if !(0.0..=255.0).contains(&value) {
        Err(".icon color channels must be in 0..255".into())
    } else {
        Ok(value.floor() as u8)
    }
}

fn flush(path: Path, canvas: f64, content: &mut String, defs: &mut String, mask_count: &mut usize) {
    if path.data.is_empty() {
        return;
    }
    if path.clear {
        // Clear removes pixels from everything drawn so far. Later draw calls
        // stay outside this mask and may paint over the cleared region again.
        let id = format!("icon_clear_{}", *mask_count);
        *mask_count += 1;
        write!(defs, "<mask id=\"{id}\" maskUnits=\"userSpaceOnUse\" maskContentUnits=\"userSpaceOnUse\" x=\"0\" y=\"0\" width=\"{}\" height=\"{}\" style=\"mask-type:luminance\"><rect width=\"{}\" height=\"{}\" fill=\"white\"/>{}</mask>",
            number(canvas), number(canvas), number(canvas), number(canvas), path.markup(true)).unwrap();
        *content = format!("<g mask=\"url(#{id})\">{content}</g>");
    } else {
        content.push_str(&path.markup(false));
    }
}

/// Export the default (last) representation. None keeps the canvas transparent;
/// an explicit backdrop is only for a preview surface. RTL-dependent commands
/// are rejected because no rendering direction is supplied by this interface.
pub(super) fn convert(source: &str, backdrop: Option<&str>) -> Result<String, String> {
    convert_with_color(source, backdrop, [0x42, 0x42, 0x42, 255])
}

/// Use the RGBA color supplied by the Chromium caller as each path's initial
/// paint. PATH_COLOR_ALPHA replaces its alpha; PATH_COLOR_ARGB replaces RGBA.
pub(super) fn convert_with_color(
    source: &str,
    backdrop: Option<&str>,
    color: [u8; 4],
) -> Result<String, String> {
    let commands = parse_commands(source)?;
    let start = commands
        .iter()
        .rposition(|c| c.name == "CANVAS_DIMENSIONS")
        .unwrap_or(0);
    let commands = &commands[start..];
    let canvas = if commands[0].name == "CANVAS_DIMENSIONS" {
        let value = commands[0].args[0];
        if value < 1.0 || value > i32::MAX as f64 {
            return Err("CANVAS_DIMENSIONS must be a positive Chromium canvas size".into());
        }
        value.trunc()
    } else {
        48.0
    };
    let mut path = Path::with_color(color);
    let mut content = String::new();
    let mut defs = String::new();
    let mut mask_count = 0;
    let mut clip = None;
    for command in commands {
        let a = &command.args;
        let p = path.current;
        let last = path.last_point;
        let mut curve = false;
        match command.name.as_str() {
            "CANVAS_DIMENSIONS" => {},
            "NEW_PATH" => flush(std::mem::replace(&mut path, Path::with_color(color)), canvas, &mut content, &mut defs, &mut mask_count),
            "FILL_RULE_NONZERO" => path.nonzero = true,
            "PATH_COLOR_ALPHA" => path.color[3] = channel(a[0])?,
            "PATH_COLOR_ARGB" => path.color = [channel(a[1])?, channel(a[2])?, channel(a[3])?, channel(a[0])?],
            "PATH_MODE_CLEAR" => path.clear = true,
            "STROKE" => {
                if a[0] <= 0.0 { return Err("STROKE requires a positive width; device-space hairlines are not supported".into()); }
                path.stroke = Some(a[0]);
            }
            "CAP_SQUARE" => path.square_cap = true,
            "DISABLE_AA" => path.disable_aa = true,
            "FLIPS_IN_RTL" => return Err("FLIPS_IN_RTL requires an explicit rendering direction and cannot be exported by this interface".into()),
            "MOVE_TO" => path.move_to(Point::new(a[0], a[1])),
            "R_MOVE_TO" => path.move_to(p.offset(a[0], a[1])),
            "LINE_TO" => path.line_to(Point::new(a[0], a[1])),
            "R_LINE_TO" => path.line_to(p.offset(a[0], a[1])),
            "H_LINE_TO" => path.line_to(Point::new(a[0], last.y)),
            "R_H_LINE_TO" => path.line_to(p.offset(a[0], 0.0)),
            "V_LINE_TO" => path.line_to(Point::new(last.x, a[0])),
            "R_V_LINE_TO" => path.line_to(p.offset(0.0, a[0])),
            "CUBIC_TO" | "R_CUBIC_TO" => {
                let offset = if command.name == "R_CUBIC_TO" { p } else { Point::default() };
                path.cubic_to(offset.offset(a[0], a[1]), offset.offset(a[2], a[3]), offset.offset(a[4], a[5]));
                curve = true;
            }
            "QUADRATIC_TO" | "R_QUADRATIC_TO" => {
                let offset = if command.name == "R_QUADRATIC_TO" { p } else { Point::default() };
                path.quadratic_to(offset.offset(a[0], a[1]), offset.offset(a[2], a[3]));
                curve = true;
            }
            "CUBIC_TO_SHORTHAND" | "QUADRATIC_TO_SHORTHAND" | "R_QUADRATIC_TO_SHORTHAND" => {
                // Chromium reflects across either curve type (not just the
                // matching SVG type). Expand explicitly instead of emitting S/T.
                let control = if path.previous_is_curve {
                    Point::new(2.0 * last.x - path.last_control.x, 2.0 * last.y - path.last_control.y)
                } else { last };
                if command.name == "CUBIC_TO_SHORTHAND" {
                    path.cubic_to(control, Point::new(a[0], a[1]), Point::new(a[2], a[3]));
                } else if command.name == "R_QUADRATIC_TO_SHORTHAND" {
                    // Chromium 142 passes the computed absolute reflected point
                    // into rQuadTo. Match that target implementation, including
                    // the additional current-point offset.
                    path.quadratic_to(p.offset(control.x, control.y), p.offset(a[0], a[1]));
                } else {
                    path.quadratic_to(control, Point::new(a[0], a[1]));
                }
                curve = true;
            }
            "ARC_TO" | "R_ARC_TO" => {
                let end = if command.name == "R_ARC_TO" { last.offset(a[5], a[6]) } else { Point::new(a[5], a[6]) };
                path.arc_to(a[0], a[1], a[2], a[3] != 0.0, a[4] != 0.0, end);
            }
            "CIRCLE" => path.oval(a[0], a[1], a[2], a[2])?,
            "OVAL" => path.oval(a[0], a[1], a[2], a[3])?,
            "ROUND_RECT" => path.round_rect(a[0], a[1], a[2], a[3], a[4])?,
            "CLOSE" => path.close(),
            "CLIP" => clip = Some([a[0], a[1], a[2], a[3]]),
            _ => unreachable!("parser accepted an unknown command"),
        }
        path.previous_is_curve = curve;
    }
    flush(path, canvas, &mut content, &mut defs, &mut mask_count);
    if let Some([x, y, w, h]) = clip.filter(|r| r[2] > 0.0 && r[3] > 0.0) {
        // Chromium calls SkCanvas::clipRect without antialiasing. Ordinary SVG
        // clipping would introduce partially transparent pixels at fractional
        // boundaries that remain fully covered or excluded in Chromium.
        write!(defs, "<clipPath id=\"icon_clip\" clipPathUnits=\"userSpaceOnUse\"><rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" shape-rendering=\"crispEdges\"/></clipPath>", number(x), number(y), number(w), number(h)).unwrap();
        content = format!("<g clip-path=\"url(#icon_clip)\">{content}</g>");
    }
    let mut result = format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" viewBox=\"0 0 {} {}\" fill-rule=\"evenodd\">", number(canvas), number(canvas), number(canvas), number(canvas));
    if !defs.is_empty() {
        write!(result, "<defs>{defs}</defs>").unwrap();
    }
    if let Some(fill) = backdrop {
        write!(
            result,
            "<rect width=\"{}\" height=\"{}\" fill=\"{}\"/>",
            number(canvas),
            number(canvas),
            escape(fill)
        )
        .unwrap();
    }
    result.push_str(&content);
    result.push_str("</svg>");
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(source: &str) -> tiny_skia::Pixmap {
        render_at(source, 160)
    }

    fn render_at(source: &str, size: u32) -> tiny_skia::Pixmap {
        render_with_color(source, size, [0x42, 0x42, 0x42, 255])
    }

    fn render_with_color(source: &str, size: u32, color: [u8; 4]) -> tiny_skia::Pixmap {
        let markup = convert_with_color(source, None, color).unwrap();
        let tree = usvg::Tree::from_str(&markup, &usvg::Options::default()).unwrap();
        let mut image = tiny_skia::Pixmap::new(size, size).unwrap();
        let scale = size as f32 / tree.size().width();
        resvg::render(
            &tree,
            tiny_skia::Transform::from_scale(scale, scale),
            &mut image.as_mut(),
        );
        image
    }

    #[test]
    fn template_rgba_is_inherited_until_explicitly_overridden() {
        let theme = [20, 100, 220, 102];
        let inherited = render_with_color("CANVAS_DIMENSIONS,16,CIRCLE,8,8,6", 16, theme);
        assert_eq!(
            inherited.pixel(8, 8).unwrap(),
            tiny_skia::ColorU8::from_rgba(20, 100, 220, 102).premultiply()
        );

        let explicit_alpha = render_with_color(
            "CANVAS_DIMENSIONS,16,PATH_COLOR_ALPHA,51,CIRCLE,8,8,6",
            16,
            theme,
        );
        assert_eq!(
            explicit_alpha.pixel(8, 8).unwrap(),
            tiny_skia::ColorU8::from_rgba(20, 100, 220, 51).premultiply()
        );

        let explicit_argb = render_with_color(
            "CANVAS_DIMENSIONS,16,PATH_COLOR_ARGB,204,240,60,10,CIRCLE,8,8,6",
            16,
            theme,
        );
        assert_eq!(
            explicit_argb.pixel(8, 8).unwrap(),
            tiny_skia::ColorU8::from_rgba(240, 60, 10, 204).premultiply()
        );
    }

    #[test]
    fn new_path_restores_the_callers_rgba_after_paint_overrides() {
        let image = render_with_color(
            "CANVAS_DIMENSIONS,24,PATH_COLOR_ARGB,204,240,60,10,CIRCLE,4,12,3,NEW_PATH,PATH_COLOR_ALPHA,51,CIRCLE,12,12,3,NEW_PATH,CIRCLE,20,12,3",
            24, [20, 100, 220, 102]);
        for (x, rgba) in [
            (4, [240, 60, 10, 204]),
            (12, [20, 100, 220, 51]),
            (20, [20, 100, 220, 102]),
        ] {
            assert_eq!(
                image.pixel(x, 12).unwrap(),
                tiny_skia::ColorU8::from_rgba(rgba[0], rgba[1], rgba[2], rgba[3]).premultiply()
            );
        }
    }

    #[test]
    fn theme_color_and_alpha_do_not_fill_compound_path_holes() {
        let image = render_with_color(
            "CANVAS_DIMENSIONS,16,ROUND_RECT,0,0,16,16,4,ROUND_RECT,1,1,14,14,3",
            160,
            [20, 100, 220, 102],
        );
        assert_eq!(image.pixel(80, 80).unwrap().alpha(), 0);
        assert_eq!(image.pixel(80, 5).unwrap().alpha(), 102);
    }

    #[test]
    fn compound_primitives_share_evenodd_path() {
        let image = render("CANVAS_DIMENSIONS,16,CIRCLE,8,8,6,CIRCLE,8,8,3");
        assert_eq!(image.pixel(80, 80).unwrap().alpha(), 0);
        assert_eq!(image.pixel(80, 35).unwrap().alpha(), 255);
    }

    #[test]
    fn stroke_is_preserved_for_primitives() {
        let image = render("CANVAS_DIMENSIONS,16,STROKE,1,CIRCLE,8,8,5");
        assert_eq!(image.pixel(80, 80).unwrap().alpha(), 0);
        assert!(image.pixel(80, 30).unwrap().alpha() > 200);
    }

    #[test]
    fn alpha_and_clear_modify_existing_pixels() {
        let image = render("CANVAS_DIMENSIONS,16,PATH_COLOR_ALPHA,0,CIRCLE,8,8,5");
        assert!(image.pixels().iter().all(|p| p.alpha() == 0));
        let image =
            render("CANVAS_DIMENSIONS,16,CIRCLE,8,8,6,NEW_PATH,PATH_MODE_CLEAR,CIRCLE,8,8,3");
        assert_eq!(image.pixel(80, 80).unwrap().alpha(), 0);
        assert_eq!(image.pixel(80, 35).unwrap().alpha(), 255);
        let image = render("CANVAS_DIMENSIONS,16,CIRCLE,8,8,6,NEW_PATH,PATH_MODE_CLEAR,CIRCLE,8,8,3,NEW_PATH,PATH_COLOR_ARGB,255,255,0,0,CIRCLE,8,8,1");
        assert_eq!(image.pixel(80, 80).unwrap().red(), 255);
        assert_eq!(image.pixel(80, 60).unwrap().alpha(), 0);
    }

    #[test]
    fn clip_applies_to_all_draw_calls_and_export_stays_transparent() {
        let source = "CANVAS_DIMENSIONS,16,ROUND_RECT,0,0,16,16,0,NEW_PATH,PATH_COLOR_ARGB,255,255,0,0,CIRCLE,12,8,2,CLIP,0,0,8,16";
        let image = render(source);
        assert_eq!(image.pixel(40, 80).unwrap().alpha(), 255);
        assert_eq!(image.pixel(120, 80).unwrap().alpha(), 0);
        let exported = convert("CANVAS_DIMENSIONS,16,CIRCLE,8,8,2", None).unwrap();
        assert!(!exported.contains("<rect"));
        assert!(convert("CANVAS_DIMENSIONS,16,CIRCLE,8,8,2", Some("white"))
            .unwrap()
            .contains("<rect"));
    }

    #[test]
    fn selects_only_last_representation() {
        let markup = convert("CANVAS_DIMENSIONS,32,PATH_COLOR_ARGB,255,255,0,0,CIRCLE,16,16,10,CANVAS_DIMENSIONS,16,PATH_COLOR_ARGB,255,0,0,255,CIRCLE,8,8,2", None).unwrap();
        assert!(markup.contains("viewBox=\"0 0 16 16\""));
        assert!(!markup.contains("#FF0000"));
        let image =
            render("CANVAS_DIMENSIONS,32,CIRCLE,16,16,10,CANVAS_DIMENSIONS,16,CIRCLE,8,8,2");
        assert_eq!(image.pixel(80, 80).unwrap().alpha(), 255);
        assert_eq!(image.pixel(140, 140).unwrap().alpha(), 0);
    }

    #[test]
    fn fractional_clip_edges_have_no_partial_pixel_coverage() {
        let image = render_at(
            "CANVAS_DIMENSIONS,16,ROUND_RECT,0,0,16,16,0,CLIP,0.75,0.75,14.5,14.5",
            16,
        );
        assert!(image
            .pixels()
            .iter()
            .all(|pixel| matches!(pixel.alpha(), 0 | 255)));
        assert_eq!(image.pixel(0, 8).unwrap().alpha(), 0);
        assert_eq!(image.pixel(1, 8).unwrap().alpha(), 255);
        assert_eq!(image.pixel(14, 8).unwrap().alpha(), 255);
        assert_eq!(image.pixel(15, 8).unwrap().alpha(), 0);
    }

    #[test]
    fn numeric_arguments_use_chromium_float32_precision_before_rounding() {
        // These decimal inputs round up when converted to SkScalar before the
        // renderer truncates the canvas dimension and floors color channels.
        let source = "CANVAS_DIMENSIONS,15.9999999,PATH_COLOR_ARGB,127.999999,254.999999,0,0,ROUND_RECT,0,0,16,16,0";
        let markup = convert(source, None).unwrap();
        assert!(markup.contains("viewBox=\"0 0 16 16\""), "{markup}");
        let image = render_at(source, 16);
        let center = image.pixel(8, 8).unwrap();
        assert_eq!(center.alpha(), 128);
        assert_eq!(center.demultiply().red(), 255);
    }

    #[test]
    fn validates_command_stream_and_numeric_arguments() {
        for source in [
            "",
            "CIRCLE,8,8",
            "CIRCLE,8,nope,3",
            "CIRCLE,8,8,NaN",
            "CANVAS_DIMENSIONS,0",
            "CIRCLE,8,8,4,BOGUS",
            "PATH_COLOR_ALPHA,256,CIRCLE,8,8,4",
            "MOVE_TO,0,0,1",
            "/* unterminated",
        ] {
            assert!(convert(source, None).is_err(), "{source}");
        }
        let markup = convert("// leading comment\nCANVAS_DIMENSIONS,16,/* inline */PATH_COLOR_ARGB,0xFF,0xFF,0,0,CIRCLE,8f,8,2 // trailing\n", None).unwrap();
        assert!(markup.contains("#FF0000"));
    }

    #[test]
    fn shorthand_matches_target_chromium_control_points() {
        let markup = convert(
            "CANVAS_DIMENSIONS,16,MOVE_TO,2,4,QUADRATIC_TO,6,0,8,4,R_QUADRATIC_TO_SHORTHAND,4,0",
            None,
        )
        .unwrap();
        assert!(markup.contains("Q18 12 12 4"), "{markup}");
        let markup = convert(
            "MOVE_TO,0,0,QUADRATIC_TO,2,4,4,0,CUBIC_TO_SHORTHAND,6,2,8,0",
            None,
        )
        .unwrap();
        assert!(markup.contains("C6 -4 6 2 8 0"), "{markup}");
    }

    #[test]
    fn closed_contours_follow_chromium_absolute_and_relative_coordinates() {
        let prefix = "CANVAS_DIMENSIONS,16,MOVE_TO,2,3,LINE_TO,8,9,CLOSE,";
        for (command, expected) in [
            ("R_LINE_TO,2,1", "Z L4 4"),
            ("R_MOVE_TO,2,1", "Z M4 4"),
            ("H_LINE_TO,7", "Z L7 9"),
            ("V_LINE_TO,7", "Z L8 7"),
            ("R_ARC_TO,2,2,0,0,1,2,1", "Z A2 2 0 0 1 10 10"),
            ("QUADRATIC_TO_SHORTHAND,12,13", "Z Q8 9 12 13"),
            ("R_QUADRATIC_TO_SHORTHAND,2,1", "Z Q10 12 4 4"),
        ] {
            let markup = convert(&format!("{prefix}{command}"), None).unwrap();
            assert!(markup.contains(expected), "{command}: {markup}");
        }
        let markup = convert("ROUND_RECT,1,2,6,8,0,V_LINE_TO,4", None).unwrap();
        assert!(markup.contains("Z L7 4"), "{markup}");
    }

    #[test]
    fn paint_changes_apply_to_the_entire_path_and_reset_at_new_path() {
        let image = render(
            "CANVAS_DIMENSIONS,16,CIRCLE,4,8,3,PATH_COLOR_ARGB,255,255,0,0,NEW_PATH,CIRCLE,12,8,3",
        );
        assert_eq!(image.pixel(40, 80).unwrap().red(), 255);
        assert_eq!(image.pixel(120, 80).unwrap().red(), 0x42);
        let image = render("CANVAS_DIMENSIONS,16,FILL_RULE_NONZERO,CIRCLE,8,8,6,CIRCLE,8,8,3");
        assert_eq!(image.pixel(80, 80).unwrap().alpha(), 255);
    }

    #[test]
    fn degenerate_circle_preserves_round_stroke_dot() {
        let image = render("CANVAS_DIMENSIONS,16,STROKE,2,CIRCLE,8,8,0");
        assert_eq!(image.pixel(80, 80).unwrap().alpha(), 255);
        assert_eq!(image.pixel(100, 80).unwrap().alpha(), 0);
    }

    #[test]
    fn unsupported_device_or_direction_dependent_commands_are_explicit_errors() {
        assert!(convert("STROKE,0,CIRCLE,8,8,3", None)
            .unwrap_err()
            .contains("hairlines"));
        assert!(convert("FLIPS_IN_RTL,CIRCLE,8,8,3", None)
            .unwrap_err()
            .contains("rendering direction"));
    }
}
