use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

use crate::model::{Product, Sweep, Volume};
use crate::parser::{decode_grib2, decode_tar, DecodeError};
use crate::render::{render_sweep_png, ColorPalette, RenderOptions};

#[derive(Debug, Clone)]
pub struct BridgeOptions {
    pub raw_root: PathBuf,
    pub site_override: Option<String>,
    pub render_size_px: u32,
    pub max_images: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublishReport {
    pub site_id: String,
    pub raw_root: PathBuf,
    pub station_dir: PathBuf,
    pub volume_count: usize,
    pub sweep_count: usize,
    pub proof_files: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Image(#[from] image::ImageError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("no decodable radar volumes were found")]
    Empty,
}

pub fn decode_input_bytes(
    bytes: &[u8],
    source_name: Option<&str>,
) -> Result<Vec<Volume>, DecodeError> {
    if source_name
        .and_then(|s| Path::new(s).extension())
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("tar"))
    {
        decode_tar(bytes)
    } else {
        Ok(vec![decode_grib2(bytes, source_name)?])
    }
}

pub fn decode_input_file(path: impl AsRef<Path>) -> Result<Vec<Volume>, BridgeError> {
    let path = path.as_ref();
    let bytes = fs::read(path)?;
    Ok(decode_input_bytes(
        &bytes,
        path.file_name().and_then(|s| s.to_str()),
    )?)
}

pub fn publish_bytes(
    bytes: &[u8],
    source_name: Option<&str>,
    options: &BridgeOptions,
) -> Result<PublishReport, BridgeError> {
    let volumes = decode_input_bytes(bytes, source_name)?;
    publish_volumes(&volumes, options)
}

pub fn publish_volumes(
    volumes: &[Volume],
    options: &BridgeOptions,
) -> Result<PublishReport, BridgeError> {
    if volumes.is_empty() {
        return Err(BridgeError::Empty);
    }

    fs::create_dir_all(&options.raw_root)?;
    let site_id = site_id_for(volumes, options.site_override.as_deref());
    let station_dir = options.raw_root.join(&site_id);
    fs::create_dir_all(&station_dir)?;

    let site = volumes.iter().find_map(|volume| volume.sweeps.first());
    write_text_atomic(
        &options.raw_root.join("grlevel2.cfg"),
        &format!("Site: {site_id}\n"),
    )?;
    write_text_atomic(
        &options.raw_root.join("customradars.gis"),
        &customradars(site, &site_id),
    )?;
    write_text_atomic(
        &options.raw_root.join("radars.gis"),
        &radars_gis(site, &site_id),
    )?;

    let summary = BridgeSummary::from_volumes(volumes, &site_id);
    write_text_atomic(
        &station_dir.join("latest.json"),
        &serde_json::to_string_pretty(&summary)?,
    )?;
    write_text_atomic(
        &station_dir.join("README.txt"),
        "This folder is served in a GRLevel2/GR2Analyst-style polling layout.\n\
         PNG and JSON outputs are live proof products. Native Archive II output is not enabled yet.\n",
    )?;

    let proof_files = render_best_sweeps(volumes, &station_dir, options)?;
    write_text_atomic(&station_dir.join("dir.list"), &dir_list(&proof_files))?;
    write_text_atomic(
        &options.raw_root.join("index.html"),
        &root_index(&site_id, &summary),
    )?;
    write_text_atomic(
        &station_dir.join("index.html"),
        &station_index(&site_id, &summary, &proof_files),
    )?;

    Ok(PublishReport {
        site_id,
        raw_root: options.raw_root.clone(),
        station_dir,
        volume_count: volumes.len(),
        sweep_count: volumes.iter().map(|v| v.sweeps.len()).sum(),
        proof_files,
        notes: vec![
            "Polling directory shape is ready for GR2A testing.".to_string(),
            "Archive II emission is the remaining GR2A-ingest step.".to_string(),
        ],
    })
}

fn render_best_sweeps(
    volumes: &[Volume],
    station_dir: &Path,
    options: &BridgeOptions,
) -> Result<Vec<String>, BridgeError> {
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

    let mut files = Vec::new();
    for (volume, idx, sweep, _) in candidates.into_iter().take(options.max_images) {
        let file_name = proof_file_name(volume, idx, sweep);
        let path = station_dir.join(&file_name);
        render_sweep_png(
            sweep,
            &path,
            RenderOptions {
                size_px: options.render_size_px,
                palette: match sweep.product {
                    Product::Velocity => ColorPalette::Velocity,
                    _ => ColorPalette::Reflectivity,
                },
                transparent_background: false,
            },
        )?;
        files.push(file_name);
    }

    Ok(files)
}

fn interesting_count(sweep: &Sweep) -> usize {
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

fn proof_file_name(volume: &Volume, idx: usize, sweep: &Sweep) -> String {
    let product = sweep.product.short_name();
    let station = sanitize_component(&sweep.site.id);
    let elev = sweep.elevation_angle_deg.unwrap_or(0.0);
    let source = volume
        .source_name
        .as_deref()
        .and_then(|s| Path::new(s).file_stem())
        .and_then(|s| s.to_str())
        .map(sanitize_component)
        .unwrap_or_else(|| "jma".to_string());
    format!("{source}_{station}_{product}_sweep{idx:02}_elev{elev:.2}.png")
}

fn site_id_for(volumes: &[Volume], override_id: Option<&str>) -> String {
    let raw = override_id
        .or_else(|| volumes.iter().find_map(Volume::station_id))
        .unwrap_or("JMA1");
    let id: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .take(4)
        .collect();
    if id.is_empty() {
        "JMA1".to_string()
    } else {
        id
    }
}

fn customradars(sweep: Option<&Sweep>, site_id: &str) -> String {
    let (lat, lon, alt) = site_position(sweep);
    format!(
        "{site_id},{site_id}, {lat:.5}, {lon:.5}, {alt:.1}, 1, JP, {site_id}/JMA radar bridge\n"
    )
}

fn radars_gis(sweep: Option<&Sweep>, site_id: &str) -> String {
    let (lat, lon, alt) = site_position(sweep);
    format!("{site_id} {site_id} {lat:.5} {lon:.5} {alt:.1} 1 JP {site_id}/JMA radar bridge\n")
}

fn site_position(sweep: Option<&Sweep>) -> (f64, f64, f32) {
    sweep
        .map(|s| {
            (
                s.site.lat_deg,
                s.site.lon_deg,
                s.site.altitude_m.unwrap_or(0.0),
            )
        })
        .unwrap_or((0.0, 0.0, 0.0))
}

fn dir_list(files: &[String]) -> String {
    let mut out = String::new();
    for file in files {
        out.push_str(file);
        out.push('\n');
    }
    out.push_str("latest.json\n");
    out
}

fn root_index(site_id: &str, summary: &BridgeSummary) -> String {
    format!(
        "<!doctype html><title>JMA Radar Bridge</title><h1>JMA Radar Bridge</h1>\
         <p>Polling root is alive.</p><ul>\
         <li><a href=\"grlevel2.cfg\">grlevel2.cfg</a></li>\
         <li><a href=\"customradars.gis\">customradars.gis</a></li>\
         <li><a href=\"{site_id}/\">{site_id}</a></li>\
         </ul><p>{} sweeps available as proof products.</p>",
        summary.sweep_count
    )
}

fn station_index(site_id: &str, summary: &BridgeSummary, proof_files: &[String]) -> String {
    let mut out = format!(
        "<!doctype html><title>{site_id} JMA Radar Bridge</title><h1>{site_id}</h1>\
         <p>{} volumes, {} sweeps.</p><ul>\
         <li><a href=\"latest.json\">latest.json</a></li>\
         <li><a href=\"README.txt\">README.txt</a></li>",
        summary.volume_count, summary.sweep_count
    );
    for file in proof_files {
        out.push_str(&format!(
            "<li><a href=\"{0}\">{0}</a></li>",
            html_text(file)
        ));
    }
    out.push_str("</ul>");
    out
}

fn sanitize_component(input: &str) -> String {
    let mut out: String = input
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if out.is_empty() {
        out.push_str("jma");
    }
    out
}

fn html_text(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn write_text_atomic(path: &Path, content: &str) -> Result<(), std::io::Error> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, content)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(tmp, path)
}

#[derive(Debug, Serialize)]
struct BridgeSummary {
    site_id: String,
    volume_count: usize,
    sweep_count: usize,
    volumes: Vec<VolumeSummary>,
}

impl BridgeSummary {
    fn from_volumes(volumes: &[Volume], site_id: &str) -> Self {
        Self {
            site_id: site_id.to_string(),
            volume_count: volumes.len(),
            sweep_count: volumes.iter().map(|v| v.sweeps.len()).sum(),
            volumes: volumes.iter().map(VolumeSummary::from_volume).collect(),
        }
    }
}

#[derive(Debug, Serialize)]
struct VolumeSummary {
    source_name: Option<String>,
    reference_time_utc: String,
    station_id: Option<String>,
    sweeps: Vec<SweepSummary>,
}

impl VolumeSummary {
    fn from_volume(volume: &Volume) -> Self {
        Self {
            source_name: volume.source_name.clone(),
            reference_time_utc: volume.identification.reference_time.iso_utc(),
            station_id: volume.station_id().map(str::to_string),
            sweeps: volume.sweeps.iter().map(SweepSummary::from_sweep).collect(),
        }
    }
}

#[derive(Debug, Serialize)]
struct SweepSummary {
    product: String,
    units: String,
    elevation_angle_deg: Option<f32>,
    gate_count: usize,
    radial_count: usize,
    gate_spacing_m: f32,
    max_range_m: f32,
    non_nan_count: usize,
}

impl SweepSummary {
    fn from_sweep(sweep: &Sweep) -> Self {
        Self {
            product: sweep.product.short_name().to_string(),
            units: sweep.product.units().to_string(),
            elevation_angle_deg: sweep.elevation_angle_deg,
            gate_count: sweep.grid.gate_count,
            radial_count: sweep.grid.radial_count,
            gate_spacing_m: sweep.grid.gate_spacing_m,
            max_range_m: sweep.grid.max_range_m(),
            non_nan_count: sweep.non_nan_count(),
        }
    }
}
