use std::fs;
use std::io::{Cursor, Read};
use std::path::Path;

use thiserror::Error;

use crate::model::{
    DateTimeParts, GridDefinition, Identification, Product, RadarSite, Sweep, Volume,
};

const GRIB_MAGIC: &[u8; 4] = b"GRIB";
const END_MAGIC: &[u8; 4] = b"7777";
const GRID_TEMPLATE_AZIMUTH_RANGE: u16 = 50120;
const PRODUCT_TEMPLATE_RADAR_ELEVATION: u16 = 51022;
const DATA_TEMPLATE_RUN_LENGTH: u16 = 200;

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("I/O error while reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid GRIB2 data: {0}")]
    Invalid(String),
    #[error("unsupported JMA GRIB2 template: section {section} template {template}")]
    UnsupportedTemplate { section: u8, template: u16 },
    #[error("missing required section {0}")]
    MissingSection(u8),
    #[error("run-length decode failed: {0}")]
    RunLength(String),
    #[error("tar archive error: {0}")]
    Tar(String),
}

#[derive(Debug, Clone, Copy)]
struct SectionRef {
    number: u8,
    offset: usize,
    length: usize,
}

pub fn decode_grib2_file(path: impl AsRef<Path>) -> Result<Volume, DecodeError> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(|source| DecodeError::Io {
        path: path.display().to_string(),
        source,
    })?;
    decode_grib2(&bytes, path.file_name().and_then(|s| s.to_str()))
}

pub fn decode_grib2(bytes: &[u8], source_name: Option<&str>) -> Result<Volume, DecodeError> {
    if bytes.len() < 20 || &bytes[0..4] != GRIB_MAGIC {
        return Err(DecodeError::Invalid("missing GRIB indicator".into()));
    }
    if bytes[7] != 2 {
        return Err(DecodeError::Invalid(format!(
            "expected GRIB edition 2, got {}",
            bytes[7]
        )));
    }

    let total_length = be_u64(bytes, 8)? as usize;
    if total_length > bytes.len() {
        return Err(DecodeError::Invalid(format!(
            "message declares {total_length} bytes but input has {}",
            bytes.len()
        )));
    }
    let msg = &bytes[..total_length];
    if msg.len() < 4 || &msg[msg.len() - 4..] != END_MAGIC {
        return Err(DecodeError::Invalid("missing GRIB end marker 7777".into()));
    }

    let sections = scan_sections(msg)?;
    let identification = sections
        .iter()
        .find(|s| s.number == 1)
        .map(|s| parse_identification(section_bytes(msg, *s)))
        .transpose()?
        .ok_or(DecodeError::MissingSection(1))?;

    let mut current_grid = None;
    let mut pending_product = None;
    let mut pending_data_repr = None;
    let mut pending_bitmap = None;
    let mut sweeps = Vec::new();

    for section in sections {
        match section.number {
            1 | 2 => {}
            3 => current_grid = Some(parse_grid(section_bytes(msg, section))?),
            4 => pending_product = Some(section),
            5 => pending_data_repr = Some(section),
            6 => pending_bitmap = Some(section),
            7 => {
                let grid = current_grid.clone().ok_or(DecodeError::MissingSection(3))?;
                let product_section = pending_product.ok_or(DecodeError::MissingSection(4))?;
                let data_repr = pending_data_repr.ok_or(DecodeError::MissingSection(5))?;
                let bitmap = pending_bitmap.ok_or(DecodeError::MissingSection(6))?;

                let mut sweep = parse_product(section_bytes(msg, product_section), &grid)?;
                let values = decode_data(
                    section_bytes(msg, data_repr),
                    section_bytes(msg, bitmap),
                    section_bytes(msg, section),
                    grid.gate_count * grid.radial_count,
                )?;
                sweep.grid = grid;
                sweep.values = values;
                sweeps.push(sweep);

                pending_product = None;
                pending_data_repr = None;
                pending_bitmap = None;
            }
            8 => break,
            n => return Err(DecodeError::Invalid(format!("unexpected section {n}"))),
        }
    }

    Ok(Volume {
        source_name: source_name.map(str::to_string),
        identification,
        sweeps,
    })
}

pub fn decode_tar(bytes: &[u8]) -> Result<Vec<Volume>, DecodeError> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let entries = archive
        .entries()
        .map_err(|e| DecodeError::Tar(e.to_string()))?;
    let mut volumes = Vec::new();

    for entry in entries {
        let mut entry = entry.map_err(|e| DecodeError::Tar(e.to_string()))?;
        let path = entry
            .path()
            .map_err(|e| DecodeError::Tar(e.to_string()))?
            .to_string_lossy()
            .to_string();
        if !path.ends_with(".bin") {
            continue;
        }
        let mut data = Vec::new();
        entry
            .read_to_end(&mut data)
            .map_err(|e| DecodeError::Tar(e.to_string()))?;
        volumes.push(decode_grib2(&data, Some(&path))?);
    }

    Ok(volumes)
}

fn scan_sections(msg: &[u8]) -> Result<Vec<SectionRef>, DecodeError> {
    let mut sections = Vec::new();
    let mut pos = 16;

    while pos < msg.len() {
        if pos + 4 <= msg.len() && &msg[pos..pos + 4] == END_MAGIC {
            sections.push(SectionRef {
                number: 8,
                offset: pos,
                length: 4,
            });
            return Ok(sections);
        }
        if pos + 5 > msg.len() {
            return Err(DecodeError::Invalid("truncated section header".into()));
        }

        let length = be_u32(msg, pos)? as usize;
        let number = msg[pos + 4];
        if length < 5 {
            return Err(DecodeError::Invalid(format!(
                "section {number} has invalid length {length}"
            )));
        }
        if pos + length > msg.len() {
            return Err(DecodeError::Invalid(format!(
                "section {number} overruns message"
            )));
        }

        sections.push(SectionRef {
            number,
            offset: pos,
            length,
        });
        pos += length;
    }

    Err(DecodeError::Invalid("missing end marker".into()))
}

fn section_bytes(msg: &[u8], section: SectionRef) -> &[u8] {
    &msg[section.offset..section.offset + section.length]
}

fn parse_identification(section: &[u8]) -> Result<Identification, DecodeError> {
    require_section(section, 1, 21)?;
    Ok(Identification {
        center_id: be_u16(section, 5)?,
        subcenter_id: be_u16(section, 7)?,
        master_table_version: section[9],
        local_table_version: section[10],
        reference_time_significance: section[11],
        reference_time: DateTimeParts {
            year: be_u16(section, 12)?,
            month: section[14],
            day: section[15],
            hour: section[16],
            minute: section[17],
            second: section[18],
        },
        production_status: section[19],
        data_type: section[20],
    })
}

fn parse_grid(section: &[u8]) -> Result<GridDefinition, DecodeError> {
    require_section(section, 3, 41)?;
    let template = be_u16(section, 12)?;
    if template != GRID_TEMPLATE_AZIMUTH_RANGE {
        return Err(DecodeError::UnsupportedTemplate {
            section: 3,
            template,
        });
    }

    let gate_count = be_u32(section, 14)? as usize;
    let radial_count = be_u32(section, 18)? as usize;
    let expected = be_u32(section, 6)? as usize;
    if gate_count * radial_count != expected {
        return Err(DecodeError::Invalid(format!(
            "grid point count mismatch: {gate_count} gates * {radial_count} radials != {expected}"
        )));
    }

    Ok(GridDefinition {
        gate_count,
        radial_count,
        center_lat_deg: signed_magnitude_i32(be_u32(section, 22)?)
            .map(|v| v as f64 / 1_000_000.0)
            .ok_or_else(|| DecodeError::Invalid("missing radar latitude".into()))?,
        center_lon_deg: be_u32(section, 26)? as f64 / 1_000_000.0,
        gate_spacing_m: be_u32(section, 30)? as f32 / 1000.0,
        range_start_m: be_u32(section, 34)? as f32 / 1000.0,
        scan_mode: section[38],
        start_azimuth_deg: be_u16(section, 39)? as f32 / 100.0,
    })
}

fn parse_product(section: &[u8], grid: &GridDefinition) -> Result<Sweep, DecodeError> {
    require_section(section, 4, 60 + grid.radial_count * 4)?;
    let template = be_u16(section, 7)?;
    if template != PRODUCT_TEMPLATE_RADAR_ELEVATION {
        return Err(DecodeError::UnsupportedTemplate {
            section: 4,
            template,
        });
    }

    let product = Product::from_jma(section[9], section[10]);
    let site = RadarSite {
        id: ascii_trim(&section[24..28]),
        number: be_u16(section, 28)?,
        lat_deg: signed_magnitude_i32(be_u32(section, 14)?)
            .map(|v| v as f64 / 1_000_000.0)
            .ok_or_else(|| DecodeError::Invalid("missing site latitude".into()))?,
        lon_deg: be_u32(section, 18)? as f64 / 1_000_000.0,
        altitude_m: signed_magnitude_i16(be_u16(section, 22)?).map(|v| v as f32 / 10.0),
        magnetic_declination_deg: signed_magnitude_i16(be_u16(section, 30)?)
            .map(|v| v as f32 / 100.0),
        tx_frequency_khz: optional_u32(be_u32(section, 32)?),
        polarization: section[36],
        operation_mode: section[37],
        quality_control: section[39],
        clutter_filter: section[40],
    };

    let elevation_angle_deg = signed_magnitude_i16(be_u16(section, 41)?).map(|v| v as f32 / 100.0);
    let representative_prfs_hz = [
        signed_magnitude_i16(be_u16(section, 44)?).map(|v| v as f32 / 10.0),
        signed_magnitude_i16(be_u16(section, 46)?).map(|v| v as f32 / 10.0),
        signed_magnitude_i16(be_u16(section, 48)?).map(|v| v as f32 / 10.0),
    ];

    let mut ray_elevation_deg = Vec::with_capacity(grid.radial_count);
    let mut ray_prf_hz = Vec::with_capacity(grid.radial_count);
    for ray in 0..grid.radial_count {
        let off = 60 + ray * 4;
        ray_elevation_deg
            .push(signed_magnitude_i16(be_u16(section, off)?).map(|v| v as f32 / 100.0));
        ray_prf_hz.push(signed_magnitude_i16(be_u16(section, off + 2)?).map(|v| v as f32 / 10.0));
    }

    Ok(Sweep {
        product,
        grid: grid.clone(),
        site,
        elevation_angle_deg,
        observation_start_offset_sec: signed_magnitude_i16(be_u16(section, 50)?),
        observation_end_offset_sec: signed_magnitude_i16(be_u16(section, 52)?),
        representative_prfs_hz,
        ray_elevation_deg,
        ray_prf_hz,
        values: Vec::new(),
    })
}

fn decode_data(
    section5: &[u8],
    section6: &[u8],
    section7: &[u8],
    expected_points: usize,
) -> Result<Vec<f32>, DecodeError> {
    require_section(section5, 5, 17)?;
    require_section(section6, 6, 6)?;
    require_section(section7, 7, 5)?;

    let encoded_points = be_u32(section5, 5)? as usize;
    let template = be_u16(section5, 9)?;
    if template != DATA_TEMPLATE_RUN_LENGTH {
        return Err(DecodeError::UnsupportedTemplate {
            section: 5,
            template,
        });
    }
    if section6[5] != 255 {
        return Err(DecodeError::Invalid(format!(
            "bitmap indicator {} is not supported yet",
            section6[5]
        )));
    }
    if encoded_points != expected_points {
        return Err(DecodeError::Invalid(format!(
            "encoded point count {encoded_points} != expected {expected_points}"
        )));
    }

    let num_bits = section5[11];
    let max_value = be_u16(section5, 12)?;
    let max_level = be_u16(section5, 14)?;
    let decimal_scale = section5[16];
    let levels_start = 17;
    let levels_end = levels_start + max_level as usize * 2;
    if levels_end > section5.len() {
        return Err(DecodeError::Invalid(
            "section 5 ended before all level values".into(),
        ));
    }

    let mut level_values = Vec::with_capacity(max_level as usize + 1);
    level_values.push(f32::NAN);
    let factor = 10_f32.powi(-(decimal_scale as i32));
    for idx in 0..max_level as usize {
        let raw = be_u16(section5, levels_start + idx * 2)?;
        let value = signed_magnitude_i16(raw)
            .map(|v| v as f32 * factor)
            .unwrap_or(f32::NAN);
        level_values.push(value);
    }

    let levels = run_length_decode(&section7[5..], num_bits, max_value, expected_points)?;
    let mut values = Vec::with_capacity(expected_points);
    for level in levels {
        values.push(
            level_values
                .get(level as usize)
                .copied()
                .ok_or_else(|| DecodeError::RunLength(format!("invalid level {level}")))?,
        );
    }

    Ok(values)
}

fn run_length_decode(
    bytes: &[u8],
    num_bits: u8,
    max_value: u16,
    expected_len: usize,
) -> Result<Vec<u16>, DecodeError> {
    if num_bits == 0 || num_bits > 16 {
        return Err(DecodeError::RunLength(format!(
            "unsupported packed width {num_bits}"
        )));
    }

    let rlbase = max_value
        .checked_add(1)
        .ok_or_else(|| DecodeError::RunLength("run-length base overflow".into()))?;
    let lngu = ((1u32 << num_bits) - u32::from(rlbase)) as usize;
    if lngu == 0 {
        return Err(DecodeError::RunLength("invalid run-length base".into()));
    }

    let mut out = Vec::with_capacity(expected_len);
    let mut cached = None;
    let mut exp = 1usize;

    for value in BitValues::new(bytes, num_bits) {
        let value = value?;
        if value < rlbase {
            if out.len() >= expected_len {
                break;
            }
            out.push(value);
            cached = Some(value);
            exp = 1;
        } else {
            let prev = cached
                .ok_or_else(|| DecodeError::RunLength("first value is a run marker".into()))?;
            let repeat = (value - rlbase) as usize * exp;
            if out.len() + repeat > expected_len {
                return Err(DecodeError::RunLength(format!(
                    "run expands past expected length {expected_len}"
                )));
            }
            out.extend(std::iter::repeat(prev).take(repeat));
            exp = exp
                .checked_mul(lngu)
                .ok_or_else(|| DecodeError::RunLength("run exponent overflow".into()))?;
        }

        if out.len() == expected_len {
            break;
        }
    }

    if out.len() != expected_len {
        return Err(DecodeError::RunLength(format!(
            "decoded {} values, expected {expected_len}",
            out.len()
        )));
    }

    Ok(out)
}

struct BitValues<'a> {
    bytes: &'a [u8],
    width: u8,
    bit_pos: usize,
}

impl<'a> BitValues<'a> {
    fn new(bytes: &'a [u8], width: u8) -> Self {
        Self {
            bytes,
            width,
            bit_pos: 0,
        }
    }
}

impl Iterator for BitValues<'_> {
    type Item = Result<u16, DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        let total_bits = self.bytes.len() * 8;
        if self.bit_pos + self.width as usize > total_bits {
            return None;
        }

        let mut value = 0u16;
        for _ in 0..self.width {
            let byte = self.bytes[self.bit_pos / 8];
            let shift = 7 - (self.bit_pos % 8);
            value = (value << 1) | u16::from((byte >> shift) & 1);
            self.bit_pos += 1;
        }
        Some(Ok(value))
    }
}

fn require_section(section: &[u8], number: u8, min_len: usize) -> Result<(), DecodeError> {
    if section.len() < min_len {
        return Err(DecodeError::Invalid(format!(
            "section {number} too short: {} < {min_len}",
            section.len()
        )));
    }
    if section[4] != number {
        return Err(DecodeError::Invalid(format!(
            "expected section {number}, got {}",
            section[4]
        )));
    }
    Ok(())
}

fn ascii_trim(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_matches(char::from(0))
        .trim()
        .to_string()
}

fn optional_u32(value: u32) -> Option<u32> {
    if value == u32::MAX {
        None
    } else {
        Some(value)
    }
}

fn signed_magnitude_i16(raw: u16) -> Option<i16> {
    if raw == u16::MAX {
        return None;
    }
    let magnitude = (raw & 0x7fff) as i16;
    if raw & 0x8000 != 0 {
        Some(-magnitude)
    } else {
        Some(magnitude)
    }
}

fn signed_magnitude_i32(raw: u32) -> Option<i32> {
    if raw == u32::MAX {
        return None;
    }
    let magnitude = (raw & 0x7fff_ffff) as i32;
    if raw & 0x8000_0000 != 0 {
        Some(-magnitude)
    } else {
        Some(magnitude)
    }
}

fn be_u16(bytes: &[u8], offset: usize) -> Result<u16, DecodeError> {
    if offset + 2 > bytes.len() {
        return Err(DecodeError::Invalid(format!(
            "u16 read past end at offset {offset}"
        )));
    }
    Ok(u16::from_be_bytes([bytes[offset], bytes[offset + 1]]))
}

fn be_u32(bytes: &[u8], offset: usize) -> Result<u32, DecodeError> {
    if offset + 4 > bytes.len() {
        return Err(DecodeError::Invalid(format!(
            "u32 read past end at offset {offset}"
        )));
    }
    Ok(u32::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ]))
}

fn be_u64(bytes: &[u8], offset: usize) -> Result<u64, DecodeError> {
    if offset + 8 > bytes.len() {
        return Err(DecodeError::Invalid(format!(
            "u64 read past end at offset {offset}"
        )));
    }
    Ok(u64::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_signed_magnitude() {
        assert_eq!(signed_magnitude_i16(0x0032), Some(50));
        assert_eq!(signed_magnitude_i16(0x8032), Some(-50));
        assert_eq!(signed_magnitude_i16(0xffff), None);
    }

    #[test]
    fn decodes_jma_run_length_example() {
        let input: Vec<u8> = vec![3, 9, 12, 6, 4, 15, 2, 1, 0, 13, 12, 2, 3]
            .into_iter()
            .map(|n| n + 240)
            .collect();
        let expected: Vec<u16> = vec![
            3, 9, 9, 6, 4, 4, 4, 4, 4, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3,
        ]
        .into_iter()
        .map(|n| n + 240)
        .collect();
        assert_eq!(
            run_length_decode(&input, 8, 250, expected.len()).unwrap(),
            expected
        );
    }
}
