use std::fs::File;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use svg::node::element::path::{Command, Data};
use svg::node::element::tag::Type;
use svg::node::element::Path as SvgPath;
use svg::node::Value;
use svg::parser::Event;
use svg::Document;

fn format_number(num: f32) -> String {
    if num.fract() == 0.0 {
        format!("{}", num as i32)
    } else {
        let truncated: f32 = {
            if num.signum() == -1.0 {
                (num * 100.0).ceil() / 100.
            } else {
                num
            }
        };
        format!("{:.2}", truncated)
            .trim_end_matches('0')
            .to_string()
            + "f"
    }
}

fn color_to_argb(color: &str) -> String {
    let color = color.trim();
    let color = color.strip_prefix("#").unwrap_or(color);

    let argb = match color.len() {
        6 => {
            let r = &color[0..2];
            let g = &color[2..4];
            let b = &color[4..6];
            format!("0xFF, 0x{}, 0x{}, 0x{}", r, g, b)
        }
        8 => {
            let a = &color[0..2];
            let r = &color[2..4];
            let g = &color[4..6];
            let b = &color[6..8];
            format!("0x{}, 0x{}, 0x{}, 0x{}", a, r, g, b)
        }
        _ => match color.to_lowercase().as_str() {
            "black" => "0xFF, 0x00, 0x00, 0x00".to_string(),
            "red" => "0xFF, 0xFF, 0x00, 0x00".to_string(),
            "white" => "0xFF, 0xFF, 0xFF, 0xFF".to_string(),
            "green" => "0xFF, 0x00, 0xFF, 0x00".to_string(),
            "blue" => "0xFF, 0x00, 0x00, 0xFF".to_string(),
            "yellow" => "0xFF, 0xFF, 0xFF, 0x00".to_string(),
            "cyan" => "0xFF, 0x00, 0xFF, 0xFF".to_string(),
            "magenta" => "0xFF, 0xFF, 0x00, 0xFF".to_string(),
            "gray" => "0xFF, 0x80, 0x80, 0x80".to_string(),
            "silver" => "0xFF, 0xC0, 0xC0, 0xC0".to_string(),
            "maroon" => "0xFF, 0x80, 0x00, 0x00".to_string(),
            "olive" => "0xFF, 0x80, 0x80, 0x00".to_string(),
            "purple" => "0xFF, 0x80, 0x00, 0x80".to_string(),
            "teal" => "0xFF, 0x00, 0x80, 0x80".to_string(),
            "navy" => "0xFF, 0x00, 0x00, 0x80".to_string(),
            _ => "".to_string(),
        },
    };

    argb
}

fn handle_svg_rect(
    tag_type: &Type,
    attributes: &std::collections::HashMap<String, Value>,
) -> String {
    let mut output = String::new();
    println!("{:?} {:?}", tag_type, attributes);
    let mut x = 0.;
    if let Some(data) = attributes.get("x") {
        x = data.parse::<f32>().unwrap();
    }

    let mut y = 0.;
    if let Some(data) = attributes.get("y") {
        y = data.parse::<f32>().unwrap();
    }

    let mut width = 0.;
    if let Some(data) = attributes.get("width") {
        width = data.parse::<f32>().unwrap();
    }

    let mut height = 0.;
    if let Some(data) = attributes.get("height") {
        height = data.parse::<f32>().unwrap();
    }

    let mut rx = 0.;
    if let Some(data) = attributes.get("rx") {
        rx = data.parse::<f32>().unwrap();
    }

    output.push_str("NEW_PATH,\r\n");
    if let Some(data) = attributes.get("fill") {
        let color = color_to_argb(data);
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

fn handle_svg_circle(
    tag_type: &Type,
    attributes: &std::collections::HashMap<String, Value>,
) -> String {
    let mut output = String::new();
    println!("{:?} {:?}", tag_type, attributes);
    output.push_str("NEW_PATH,\r\n");
    output.push_str("CLOSE,\r\n");
    output
}

fn handle_svg_ellipse(
    tag_type: &Type,
    attributes: &std::collections::HashMap<String, Value>,
) -> String {
    let mut output = String::new();
    println!("{:?} {:?}", tag_type, attributes);
    output.push_str("NEW_PATH,\r\n");
    output.push_str("CLOSE,\r\n");
    output
}

fn handle_svg_path(
    path: &str,
    tag_type: &Type,
    attributes: &std::collections::HashMap<String, Value>,
) -> String {
    let mut output = String::new();
    println!("{:?}", path);
    println!("{:?}", tag_type);
    println!("Processing tag: {:?}", attributes);
    if let Some(fill) = attributes.get("fill") {
        let color = color_to_argb(fill);
        if !color.is_empty() {
            output.push_str(&format!("PATH_COLOR_ARGB, {},\r\n", color));
        }
        let fill_rule = attributes.get("fill-rule");
        if fill_rule.is_none() || fill_rule != Some(&Value::from("evenodd")) {
            output.push_str("FILL_RULE_EVENODD,\r\n");
        }

        if let Some(view_box) = attributes.get("viewBox") {
            let parts: Vec<&str> = view_box.split(' ').collect();
            let width = parts[2].parse::<f64>().unwrap();
            output.push_str(&format!("CANVAS_DIMENSIONS, {},\r\n", width));
        } else if let Some(data) = attributes.get("width") {
            let width = data.parse::<f32>().unwrap();
            output.push_str(&format!("CANVAS_DIMENSIONS, {},\r\n", width));
        }

        if let Some(data) = attributes.get("d") {
            let data = Data::parse(data).unwrap();
            for (_j, command) in data.iter().enumerate() {
                println!("{:?}", command);
                match command {
                    Command::Move(position, parameters) => {
                        if parameters.len() >= 2 {
                            let (x1, y1) = (parameters[0], parameters[1]);
                            match position {
                                svg::node::element::path::Position::Absolute => {
                                    output.push_str(&format!(
                                        "MOVE_TO, {}, {},\r\n",
                                        format_number(x1),
                                        format_number(y1)
                                    ));
                                }
                                svg::node::element::path::Position::Relative => {
                                    output.push_str(&format!(
                                        "R_MOVE_TO, {}, {},\r\n",
                                        format_number(x1),
                                        format_number(y1)
                                    ));
                                }
                            }
                        }
                        if parameters.len() >= 4 {
                            let (x2, y2) = (parameters[2], parameters[3]);
                            match position {
                                svg::node::element::path::Position::Absolute => {
                                    output.push_str(&format!(
                                        "LINE_TO, {}, {},\r\n",
                                        format_number(x2),
                                        format_number(y2)
                                    ));
                                }
                                svg::node::element::path::Position::Relative => {
                                    output.push_str(&format!(
                                        "R_LINE_TO, {}, {},\r\n",
                                        format_number(x2),
                                        format_number(y2)
                                    ));
                                }
                            }
                        }
                    }
                    Command::Line(position, parameters) => {
                        let (x, y) = (parameters[0], parameters[1]);
                        match position {
                            svg::node::element::path::Position::Absolute => {
                                output.push_str(&format!(
                                    "LINE_TO, {}, {},\r\n",
                                    format_number(x),
                                    format_number(y)
                                ));
                            }
                            svg::node::element::path::Position::Relative => {
                                output.push_str(&format!(
                                    "R_LINE_TO, {}, {},\r\n",
                                    format_number(x),
                                    format_number(y)
                                ));
                            }
                        }
                    }
                    Command::HorizontalLine(position, parameters) => {
                        let x = parameters[0];
                        match position {
                            svg::node::element::path::Position::Absolute => {
                                output.push_str(&format!("H_LINE_TO, {},\r\n", format_number(x)));
                            }
                            svg::node::element::path::Position::Relative => {
                                output.push_str(&format!("R_H_LINE_TO, {},\r\n", format_number(x)));
                            }
                        }
                    }
                    Command::VerticalLine(position, parameters) => {
                        let y = parameters[0];
                        match position {
                            svg::node::element::path::Position::Absolute => {
                                output.push_str(&format!("V_LINE_TO, {},\r\n", format_number(y)));
                            }
                            svg::node::element::path::Position::Relative => {
                                output.push_str(&format!("R_V_LINE_TO, {},\r\n", format_number(y)));
                            }
                        }
                    }
                    Command::QuadraticCurve(position, parameters) => {
                        let (x1, y1, x, y) =
                            (parameters[0], parameters[1], parameters[2], parameters[3]);
                        match position {
                            svg::node::element::path::Position::Absolute => {
                                output.push_str(&format!(
                                    "QUADRATIC_TO, {}, {}, {}, {},\r\n",
                                    format_number(x1),
                                    format_number(y1),
                                    format_number(x),
                                    format_number(y)
                                ));
                            }
                            svg::node::element::path::Position::Relative => {
                                output.push_str(&format!(
                                    "R_QUADRATIC_TO, {}, {}, {}, {},\r\n",
                                    format_number(x1),
                                    format_number(y1),
                                    format_number(x),
                                    format_number(y)
                                ));
                            }
                        }
                    }
                    Command::SmoothQuadraticCurve(position, parameters) => {
                        let (x, y) = (parameters[0], parameters[1]);
                        match position {
                            svg::node::element::path::Position::Absolute => {
                                output.push_str(&format!(
                                    "SMOOTH_QUADRATIC_TO, {}, {},\r\n",
                                    format_number(x),
                                    format_number(y)
                                ));
                            }
                            svg::node::element::path::Position::Relative => {
                                output.push_str(&format!(
                                    "R_SMOOTH_QUADRATIC_TO, {}, {},\r\n",
                                    format_number(x),
                                    format_number(y)
                                ));
                            }
                        }
                    }
                    Command::CubicCurve(position, parameters) => {
                        let (x1, y1, x2, y2, x, y) = (
                            parameters[0],
                            parameters[1],
                            parameters[2],
                            parameters[3],
                            parameters[4],
                            parameters[5],
                        );
                        match position {
                            svg::node::element::path::Position::Absolute => {
                                output.push_str(&format!(
                                    "CUBIC_TO, {}, {}, {}, {}, {}, {},\r\n",
                                    format_number(x1),
                                    format_number(y1),
                                    format_number(x2),
                                    format_number(y2),
                                    format_number(x),
                                    format_number(y)
                                ));
                            }
                            svg::node::element::path::Position::Relative => {
                                output.push_str(&format!(
                                    "R_CUBIC_TO, {}, {}, {}, {}, {}, {},\r\n",
                                    format_number(x1),
                                    format_number(y1),
                                    format_number(x2),
                                    format_number(y2),
                                    format_number(x),
                                    format_number(y)
                                ));
                            }
                        }
                    }
                    Command::SmoothCubicCurve(position, parameters) => {
                        let (x2, y2, x, y) =
                            (parameters[0], parameters[1], parameters[2], parameters[3]);
                        match position {
                            svg::node::element::path::Position::Absolute => {
                                output.push_str(&format!(
                                    "SMOOTH_CUBIC_TO, {}, {}, {}, {},\r\n",
                                    format_number(x2),
                                    format_number(y2),
                                    format_number(x),
                                    format_number(y)
                                ));
                            }
                            svg::node::element::path::Position::Relative => {
                                output.push_str(&format!(
                                    "R_SMOOTH_CUBIC_TO, {}, {}, {}, {},\r\n",
                                    format_number(x2),
                                    format_number(y2),
                                    format_number(x),
                                    format_number(y)
                                ));
                            }
                        }
                    }
                    Command::EllipticalArc(position, parameters) => {
                        let (rx, ry, x_axis_rotation, large_arc_flag, sweep_flag, x, y) = (
                            parameters[0],
                            parameters[1],
                            parameters[2],
                            parameters[3],
                            parameters[4],
                            parameters[5],
                            parameters[6],
                        );
                        match position {
                            svg::node::element::path::Position::Absolute => {
                                output.push_str(&format!(
                                    "ARC_TO, {}, {}, {}, {}, {}, {}, {},\r\n",
                                    format_number(rx),
                                    format_number(ry),
                                    format_number(x_axis_rotation),
                                    format_number(large_arc_flag),
                                    format_number(sweep_flag),
                                    format_number(x),
                                    format_number(y)
                                ));
                            }
                            svg::node::element::path::Position::Relative => {
                                output.push_str(&format!(
                                    "R_ARC_TO, {}, {}, {}, {}, {}, {}, {},\r\n",
                                    format_number(rx),
                                    format_number(ry),
                                    format_number(x_axis_rotation),
                                    format_number(large_arc_flag),
                                    format_number(sweep_flag),
                                    format_number(x),
                                    format_number(y)
                                ));
                            }
                        }
                    }
                    Command::Close => {
                        output.push_str("CLOSE,\r\n");
                    }
                }
            }
        }
    }
    output
}

pub fn convert_svg_to_chromium_icon(svg_path: &str, output_path: &str) -> String {
    let mut content = String::new();
    let dst = PathBuf::from(Path::new(svg_path).parent().unwrap()).join(output_path);
    let mut output_file = File::create(dst.clone()).expect("Failed to create output file");

    writeln!(output_file, "// Copyright 2015 The Chromium Authors")
        .expect("Failed to write to output file");
    writeln!(
        output_file,
        "// Use of this source code is governed by a BSD-style license that can be"
    )
    .expect("Failed to write to output file");
    writeln!(output_file, "// found in the LICENSE file.").expect("Failed to write to output file");
    writeln!(output_file, "").expect("Failed to write to output file");

    let mut output = String::new();
    let events = svg::open(svg_path, &mut content)
        .unwrap()
        .collect::<Vec<_>>();

    let mut canvas_dimensions: f64 = 0.0;
    for (_i, event) in events.iter().enumerate() {
        match event {
            Event::Tag(_, _, attributes) => {
                if let Some(view_box) = attributes.get("viewBox") {
                    let parts: Vec<&str> = view_box.split(' ').collect();
                    canvas_dimensions = parts[2].parse::<f64>().unwrap() as f64;
                    break;
                } else if let Some(data) = attributes.get("width") {
                    canvas_dimensions = data.parse::<f64>().unwrap() as f64;
                    break;
                }
            }
            _ => {}
        }
    }

    writeln!(output_file, "CANVAS_DIMENSIONS, {},", canvas_dimensions)
        .expect("Failed to write to output file");

    for (i, event) in events.iter().enumerate() {
        if i != 0 && !output.is_empty() {
            output.push_str("NEW_PATH,\r\n");
        }
        match event {
            Event::Tag("g", _type, attributes) => {
                if let Some(_transform) = attributes.get("transform") {
                    println!("<g> tag not support transform"); // 添加调试信息
                                                               //break;
                }
                println!("<g> tag not process");
            }
            Event::Tag("path", tag_type, attributes) => {
                println!("{:?}", attributes);
                let data = handle_svg_path("path", tag_type, attributes);
                writeln!(output_file, "{}", data).expect("Failed to write to output file");
                
            }
            Event::Tag("circle", tag_type, attributes) => {
                println!("{:?}", attributes);
                let data = handle_svg_circle(tag_type, attributes);
                writeln!(output_file, "{}", data).expect("Failed to write to output file");
            }
            Event::Tag("rect", tag_type, attributes) => {
                println!("{:?}", attributes);
                let data = handle_svg_rect(tag_type, attributes);
                writeln!(output_file, "{}", data).expect("Failed to write to output file");
            }
            Event::Tag("ellipse", tag_type, attributes) => {
                println!("{:?}", attributes);
                let data = handle_svg_ellipse(tag_type, attributes);
                writeln!(output_file, "{}", data).expect("Failed to write to output file");
            }
            _ => {}
        }
    }
    dst.to_str().unwrap().to_string()
}

#[allow(dead_code)]
pub fn convert_chromium_icon_to_svg(icon_path: &str, output_path: &str) {
    let file = File::open(icon_path).expect("Failed to open input file");
    let reader = io::BufReader::new(file);

    let mut data = Data::new();
    let mut canvas_dimensions = 24;

    for line in reader.lines() {
        let line = line.expect("Failed to read line");
        println!("Processing line: {}", line); // 添加调试信息
        let parts: Vec<&str> = line
            .split(',')
            .map(|s| s.trim().trim_end_matches('f'))
            .collect();

        match parts[0] {
            "CANVAS_DIMENSIONS" => {
                canvas_dimensions = parts[1].parse().expect("Failed to parse canvas dimensions");
            }
            "MOVE_TO" => {
                let x: f32 = parts[1].parse().expect("Failed to parse x");
                let y: f32 = parts[2].parse().expect("Failed to parse y");
                println!("MOVE_TO: ({}, {})", x, y); // 调试输出
                data = data.move_to((x, y));
            }
            "R_MOVE_TO" => {
                let x: f32 = parts[1].parse().expect("Failed to parse x");
                let y: f32 = parts[2].parse().expect("Failed to parse y");
                println!("R_MOVE_TO: ({}, {})", x, y); // 调试输出
                data = data.move_by((x, y));
            }
            "LINE_TO" => {
                let x: f32 = parts[1].parse().expect("Failed to parse x");
                let y: f32 = parts[2].parse().expect("Failed to parse y");
                println!("LINE_TO: ({}, {})", x, y); // 调试输出
                data = data.line_to((x, y));
            }
            "R_LINE_TO" => {
                let x: f32 = parts[1].parse().expect("Failed to parse x");
                let y: f32 = parts[2].parse().expect("Failed to parse y");
                println!("R_LINE_TO: ({}, {})", x, y); // 调试输出
                data = data.line_by((x, y));
            }
            "H_LINE_TO" => {
                let x: f32 = parts[1].parse().expect("Failed to parse x");
                println!("H_LINE_TO: ({})", x); // 调试输出
                data = data.horizontal_line_to(x);
            }
            "R_H_LINE_TO" => {
                let x: f32 = parts[1].parse().expect("Failed to parse x");
                println!("R_H_LINE_TO: ({})", x); // 调试输出
                data = data.horizontal_line_by(x);
            }
            "V_LINE_TO" => {
                let y: f32 = parts[1].parse().expect("Failed to parse y");
                println!("V_LINE_TO: ({})", y); // 调试输出
                data = data.vertical_line_to(y);
            }
            "R_V_LINE_TO" => {
                let y: f32 = parts[1].parse().expect("Failed to parse y");
                println!("R_V_LINE_TO: ({})", y); // 调试输出
                data = data.vertical_line_by(y);
            }
            "QUADRATIC_TO" => {
                let x1: f32 = parts[1].parse().expect("Failed to parse x1");
                let y1: f32 = parts[2].parse().expect("Failed to parse y1");
                let x: f32 = parts[3].parse().expect("Failed to parse x");
                let y: f32 = parts[4].parse().expect("Failed to parse y");
                println!("QUADRATIC_TO: ({}, {}), ({}, {})", x1, y1, x, y); // 调试输出
                data = data.quadratic_curve_to((x1, y1, x, y));
            }
            "R_QUADRATIC_TO" => {
                let x1: f32 = parts[1].parse().expect("Failed to parse x1");
                let y1: f32 = parts[2].parse().expect("Failed to parse y1");
                let x: f32 = parts[3].parse().expect("Failed to parse x");
                let y: f32 = parts[4].parse().expect("Failed to parse y");
                println!("R_QUADRATIC_TO: ({}, {}), ({}, {})", x1, y1, x, y); // 调试输出
                data = data.quadratic_curve_by((x1, y1, x, y));
            }
            "ARC_TO" => {
                let rx: f32 = parts[1].parse().expect("Failed to parse rx");
                let ry: f32 = parts[2].parse().expect("Failed to parse ry");
                let x_axis_rotation: f32 =
                    parts[3].parse().expect("Failed to parse x_axis_rotation");
                let large_arc_flag: f32 = if parts[4]
                    .parse::<u8>()
                    .expect("Failed to parse large_arc_flag")
                    != 0
                {
                    1.0
                } else {
                    0.0
                };
                let sweep_flag: f32 =
                    if parts[5].parse::<u8>().expect("Failed to parse sweep_flag") != 0 {
                        1.0
                    } else {
                        0.0
                    };
                let x: f32 = parts[6].parse().expect("Failed to parse x");
                let y: f32 = parts[7].parse().expect("Failed to parse y");
                println!(
                    "ARC_TO: ({}, {}), {}, {}, {}, ({}, {})",
                    rx, ry, x_axis_rotation, large_arc_flag, sweep_flag, x, y
                ); // 调试输出
                data = data.elliptical_arc_to((
                    rx,
                    ry,
                    x_axis_rotation,
                    large_arc_flag,
                    sweep_flag,
                    x,
                    y,
                ));
            }
            "R_ARC_TO" => {
                let rx: f32 = parts[1].parse().expect("Failed to parse rx");
                let ry: f32 = parts[2].parse().expect("Failed to parse ry");
                let x_axis_rotation: f32 =
                    parts[3].parse().expect("Failed to parse x_axis_rotation");
                let large_arc_flag: f32 = if parts[4]
                    .parse::<u8>()
                    .expect("Failed to parse large_arc_flag")
                    != 0
                {
                    1.0
                } else {
                    0.0
                };
                let sweep_flag: f32 =
                    if parts[5].parse::<u8>().expect("Failed to parse sweep_flag") != 0 {
                        1.0
                    } else {
                        0.0
                    };
                let x: f32 = parts[6].parse().expect("Failed to parse x");
                let y: f32 = parts[7].parse().expect("Failed to parse y");
                println!(
                    "R_ARC_TO: ({}, {}), {}, {}, {}, ({}, {})",
                    rx, ry, x_axis_rotation, large_arc_flag, sweep_flag, x, y
                ); // 调试输出
                data = data.elliptical_arc_by((
                    rx,
                    ry,
                    x_axis_rotation,
                    large_arc_flag,
                    sweep_flag,
                    x,
                    y,
                ));
            }
            "CLOSE" => {
                println!("CLOSE"); // 调试输出
                data = data.close();
            }
            _ => {
                println!("Unknown command: {}", parts[0]); // 调试输出
            }
        }
    }

    let path = SvgPath::new()
        .set("fill", "none")
        .set("stroke", "black")
        .set("stroke-width", 1)
        .set("d", data);

    let document = Document::new()
        .set("viewBox", (0, 0, canvas_dimensions, canvas_dimensions))
        .add(path);

    svg::save(output_path, &document).expect("Failed to save SVG file");
}
