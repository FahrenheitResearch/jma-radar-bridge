use std::fs;
use std::path::{Path, PathBuf};

use clap::Parser;
use jma_radar_rs::{
    decode_grib2, decode_tar, render_sweep_png, ColorPalette, Product, RenderOptions, Sweep, Volume,
};

#[derive(Debug, Parser)]
#[command(about = "Render quick-look PNGs from JMA polar-coordinate radar GRIB2")]
struct Args {
    input: PathBuf,
    #[arg(long, default_value = "proof")]
    out_dir: PathBuf,
    #[arg(long, default_value_t = 1200)]
    size: u32,
    #[arg(long, default_value_t = 8)]
    max_images: usize,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    fs::create_dir_all(&args.out_dir)?;
    let bytes = fs::read(&args.input)?;
    let volumes = if args
        .input
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("tar"))
    {
        decode_tar(&bytes)?
    } else {
        vec![decode_grib2(
            &bytes,
            args.input.file_name().and_then(|s| s.to_str()),
        )?]
    };

    let rendered = render_best_sweeps(&volumes, &args.out_dir, args.size, args.max_images)?;

    eprintln!("rendered {rendered} PNG(s) to {}", args.out_dir.display());
    Ok(())
}

fn render_best_sweeps(
    volumes: &[Volume],
    out_dir: &Path,
    size: u32,
    max_images: usize,
) -> Result<usize, Box<dyn std::error::Error>> {
    let mut candidates: Vec<_> = volumes
        .iter()
        .flat_map(|volume| {
            volume
                .sweeps
                .iter()
                .enumerate()
                .map(move |(idx, sweep)| (volume, idx, sweep, interesting_count(sweep)))
        })
        .filter(|(_, _, _, score)| *score > 0)
        .collect();
    candidates.sort_by_key(|(_, _, _, score)| std::cmp::Reverse(*score));

    let mut rendered = 0usize;
    for (volume, idx, sweep, _) in candidates.into_iter().take(max_images) {
        let file_name = output_file_name(volume, idx, sweep);
        let path = out_dir.join(file_name);
        render_sweep_png(
            sweep,
            path,
            RenderOptions {
                size_px: size,
                palette: match sweep.product {
                    Product::Velocity => ColorPalette::Velocity,
                    _ => ColorPalette::Reflectivity,
                },
                transparent_background: false,
            },
        )?;
        rendered += 1;
    }

    Ok(rendered)
}

fn output_file_name(volume: &Volume, idx: usize, sweep: &Sweep) -> String {
    let product = sweep.product.short_name();
    let station = sanitize(&sweep.site.id);
    let elev = sweep.elevation_angle_deg.unwrap_or(0.0);
    let source = volume
        .source_name
        .as_deref()
        .and_then(|s| Path::new(s).file_stem())
        .and_then(|s| s.to_str())
        .map(sanitize)
        .unwrap_or_else(|| "jma".to_string());
    format!("{source}_{station}_{product}_sweep{idx:02}_elev{elev:.2}.png")
}

fn sanitize(input: &str) -> String {
    input
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn interesting_count(sweep: &jma_radar_rs::Sweep) -> usize {
    sweep
        .values
        .iter()
        .filter(|value| {
            !value.is_nan()
                && match sweep.product {
                    Product::Reflectivity => **value >= 5.0,
                    Product::Velocity => value.abs() >= 0.5,
                    Product::Unknown { .. } => true,
                }
        })
        .count()
}
