use std::path::Path;

use image::{ImageBuffer, ImageError, Rgba};

use crate::model::{Product, Sweep};

#[derive(Debug, Clone, Copy)]
pub struct RenderOptions {
    pub size_px: u32,
    pub palette: ColorPalette,
    pub transparent_background: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            size_px: 1200,
            palette: ColorPalette::Auto,
            transparent_background: false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ColorPalette {
    Auto,
    Reflectivity,
    Velocity,
}

pub fn render_sweep_png(
    sweep: &Sweep,
    path: impl AsRef<Path>,
    options: RenderOptions,
) -> Result<(), ImageError> {
    let img = render_sweep(sweep, options);
    img.save(path)
}

pub fn render_sweep(sweep: &Sweep, options: RenderOptions) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
    let size = options.size_px.max(64);
    let mut img = ImageBuffer::from_pixel(
        size,
        size,
        if options.transparent_background {
            Rgba([0, 0, 0, 0])
        } else {
            Rgba([4, 7, 12, 255])
        },
    );

    let center = (size as f32 - 1.0) * 0.5;
    let max_range = sweep.grid.max_range_m().max(1.0);
    let meters_per_pixel = max_range / center;
    let az_step = 360.0 / sweep.grid.radial_count as f32;

    for y in 0..size {
        let dy = center - y as f32;
        for x in 0..size {
            let dx = x as f32 - center;
            let range_m = (dx * dx + dy * dy).sqrt() * meters_per_pixel;
            if range_m < sweep.grid.range_start_m || range_m >= max_range {
                continue;
            }

            let mut az = dx.atan2(dy).to_degrees();
            if az < 0.0 {
                az += 360.0;
            }

            let rel = if sweep.grid.scans_clockwise() {
                (az - sweep.grid.start_azimuth_deg).rem_euclid(360.0)
            } else {
                (sweep.grid.start_azimuth_deg - az).rem_euclid(360.0)
            };
            let ray = (rel / az_step).round() as usize % sweep.grid.radial_count;
            let gate = ((range_m - sweep.grid.range_start_m) / sweep.grid.gate_spacing_m) as usize;

            if let Some(value) = sweep.value(ray, gate) {
                if value.is_nan() {
                    continue;
                }
                let color = color_for_value(value, &sweep.product, options.palette);
                if color[3] != 0 {
                    img.put_pixel(x, y, Rgba(color));
                }
            }
        }
    }

    draw_range_rings(&mut img, options.transparent_background);
    img
}

fn color_for_value(value: f32, product: &Product, palette: ColorPalette) -> [u8; 4] {
    let palette = match palette {
        ColorPalette::Auto => match product {
            Product::Velocity => ColorPalette::Velocity,
            _ => ColorPalette::Reflectivity,
        },
        other => other,
    };

    match palette {
        ColorPalette::Reflectivity | ColorPalette::Auto => reflectivity_color(value),
        ColorPalette::Velocity => velocity_color(value),
    }
}

fn reflectivity_color(v: f32) -> [u8; 4] {
    if v < 5.0 {
        return [0, 0, 0, 0];
    }
    let table: &[(f32, [u8; 3])] = &[
        (-20.0, [58, 77, 118]),
        (-5.0, [42, 119, 160]),
        (5.0, [30, 170, 110]),
        (15.0, [40, 195, 70]),
        (25.0, [210, 210, 40]),
        (35.0, [242, 145, 35]),
        (45.0, [220, 40, 35]),
        (55.0, [180, 45, 140]),
        (65.0, [245, 245, 245]),
    ];
    ramp_color(v, table)
}

fn velocity_color(v: f32) -> [u8; 4] {
    let table: &[(f32, [u8; 3])] = &[
        (-45.0, [20, 55, 180]),
        (-30.0, [35, 120, 215]),
        (-15.0, [25, 175, 110]),
        (-3.0, [145, 215, 125]),
        (0.0, [38, 43, 48]),
        (3.0, [230, 195, 110]),
        (15.0, [230, 95, 45]),
        (30.0, [190, 35, 45]),
        (45.0, [245, 235, 245]),
    ];
    ramp_color(v, table)
}

fn ramp_color(v: f32, table: &[(f32, [u8; 3])]) -> [u8; 4] {
    if table.is_empty() {
        return [255, 255, 255, 255];
    }
    if v <= table[0].0 {
        let c = table[0].1;
        return [c[0], c[1], c[2], 255];
    }
    for pair in table.windows(2) {
        let (v0, c0) = pair[0];
        let (v1, c1) = pair[1];
        if v <= v1 {
            let t = ((v - v0) / (v1 - v0)).clamp(0.0, 1.0);
            return [
                lerp(c0[0], c1[0], t),
                lerp(c0[1], c1[1], t),
                lerp(c0[2], c1[2], t),
                255,
            ];
        }
    }
    let c = table[table.len() - 1].1;
    [c[0], c[1], c[2], 255]
}

fn lerp(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).round() as u8
}

fn draw_range_rings(img: &mut ImageBuffer<Rgba<u8>, Vec<u8>>, transparent: bool) {
    if transparent {
        return;
    }
    let size = img.width();
    let center = (size as f32 - 1.0) * 0.5;
    let rings = [0.25_f32, 0.5, 0.75, 1.0];
    for y in 0..size {
        let dy = center - y as f32;
        for x in 0..size {
            let dx = x as f32 - center;
            let r = (dx * dx + dy * dy).sqrt() / center;
            if rings.iter().any(|ring| (r - ring).abs() < 0.0018) {
                let pixel = img.get_pixel_mut(x, y);
                let bg = pixel.0;
                pixel.0 = [
                    bg[0].saturating_add(22),
                    bg[1].saturating_add(24),
                    bg[2].saturating_add(28),
                    255,
                ];
            }
        }
    }
}
