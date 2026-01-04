use std::fs;
use resvg::usvg::{Tree, Options};
use resvg::tiny_skia::{Pixmap, Transform};

pub fn convert_svg_to_png(svg_path: &str, out_path: &str) -> String {
    let svg_data = fs::read(svg_path).expect("Failed to read SVG file");
    let opt = Options::default();
    let rtree = Tree::from_data(&svg_data, &opt).expect("Failed to parse SVG data");
  
    let size = rtree.size();
    let width = size.width().ceil() as u32;
    let height = size.height().ceil() as u32;

    let mut pixmap = Pixmap::new(width, height).expect("Failed to create Pixmap");

    let svg_size = rtree.size();
    let scale_x = width as f32 / svg_size.width() as f32;
    let scale_y = height as f32 / svg_size.height() as f32;
    let scale = scale_x.min(scale_y);

    let transform = Transform::from_scale(scale, scale);

    resvg::render(
        &rtree,
        transform,
        &mut pixmap.as_mut(),
    );
    let output_path = std::path::Path::new(svg_path).parent().unwrap().join(out_path);
    pixmap.save_png(output_path.clone()).expect("Failed to save PNG file");
    output_path.to_str().unwrap().to_string()
}