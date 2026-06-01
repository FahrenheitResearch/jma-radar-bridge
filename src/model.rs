use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identification {
    pub center_id: u16,
    pub subcenter_id: u16,
    pub master_table_version: u8,
    pub local_table_version: u8,
    pub reference_time_significance: u8,
    pub reference_time: DateTimeParts,
    pub production_status: u8,
    pub data_type: u8,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct DateTimeParts {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl DateTimeParts {
    pub fn compact_utc(&self) -> String {
        format!(
            "{:04}{:02}{:02}{:02}{:02}{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    pub fn iso_utc(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Volume {
    pub source_name: Option<String>,
    pub identification: Identification,
    pub sweeps: Vec<Sweep>,
}

impl Volume {
    pub fn station_id(&self) -> Option<&str> {
        self.sweeps.first().map(|s| s.site.id.as_str())
    }

    pub fn station_number(&self) -> Option<u16> {
        self.sweeps.first().map(|s| s.site.number)
    }

    pub fn to_american_volume(&self) -> AmericanVolume {
        AmericanVolume {
            source_name: self.source_name.clone(),
            station_id: self.station_id().unwrap_or("JMA").to_string(),
            station_number: self.station_number(),
            reference_time: self.identification.reference_time,
            sweeps: self.sweeps.iter().map(Sweep::to_american_sweep).collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sweep {
    pub product: Product,
    pub grid: GridDefinition,
    pub site: RadarSite,
    pub elevation_angle_deg: Option<f32>,
    pub observation_start_offset_sec: Option<i16>,
    pub observation_end_offset_sec: Option<i16>,
    pub representative_prfs_hz: [Option<f32>; 3],
    pub ray_elevation_deg: Vec<Option<f32>>,
    pub ray_prf_hz: Vec<Option<f32>>,
    pub values: Vec<f32>,
}

impl Sweep {
    pub fn value(&self, ray: usize, gate: usize) -> Option<f32> {
        if ray >= self.grid.radial_count || gate >= self.grid.gate_count {
            return None;
        }
        Some(self.values[ray * self.grid.gate_count + gate])
    }

    pub fn ray_azimuth_deg(&self, ray: usize) -> f32 {
        let step = 360.0 / self.grid.radial_count as f32;
        let signed_step = if self.grid.scans_clockwise() {
            step
        } else {
            -step
        };
        (self.grid.start_azimuth_deg + signed_step * ray as f32).rem_euclid(360.0)
    }

    pub fn non_nan_count(&self) -> usize {
        self.values.iter().filter(|v| !v.is_nan()).count()
    }

    pub fn to_american_sweep(&self) -> AmericanSweep {
        let mut radials = Vec::with_capacity(self.grid.radial_count);
        for ray in 0..self.grid.radial_count {
            let offset = ray * self.grid.gate_count;
            radials.push(AmericanRadial {
                azimuth_deg: self.ray_azimuth_deg(ray),
                elevation_deg: self
                    .ray_elevation_deg
                    .get(ray)
                    .copied()
                    .flatten()
                    .or(self.elevation_angle_deg)
                    .unwrap_or(0.0),
                moments: vec![AmericanMoment {
                    name: self.product.short_name().to_string(),
                    units: self.product.units().to_string(),
                    first_gate_range_m: self.grid.range_start_m,
                    gate_size_m: self.grid.gate_spacing_m,
                    data: self.values[offset..offset + self.grid.gate_count].to_vec(),
                }],
            });
        }

        AmericanSweep {
            product: self.product.short_name().to_string(),
            elevation_angle_deg: self.elevation_angle_deg,
            radials,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GridDefinition {
    pub gate_count: usize,
    pub radial_count: usize,
    pub center_lat_deg: f64,
    pub center_lon_deg: f64,
    pub gate_spacing_m: f32,
    pub range_start_m: f32,
    pub scan_mode: u8,
    pub start_azimuth_deg: f32,
}

impl GridDefinition {
    pub fn max_range_m(&self) -> f32 {
        self.range_start_m + self.gate_count as f32 * self.gate_spacing_m
    }

    pub fn scans_clockwise(&self) -> bool {
        self.scan_mode & 0b0100_0000 == 0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RadarSite {
    pub id: String,
    pub number: u16,
    pub lat_deg: f64,
    pub lon_deg: f64,
    pub altitude_m: Option<f32>,
    pub magnetic_declination_deg: Option<f32>,
    pub tx_frequency_khz: Option<u32>,
    pub polarization: u8,
    pub operation_mode: u8,
    pub quality_control: u8,
    pub clutter_filter: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Product {
    Reflectivity,
    Velocity,
    Unknown { category: u8, number: u8 },
}

impl Product {
    pub fn from_jma(category: u8, number: u8) -> Self {
        match (category, number) {
            (15, 1) => Self::Reflectivity,
            (15, 2) => Self::Velocity,
            _ => Self::Unknown { category, number },
        }
    }

    pub fn short_name(&self) -> &'static str {
        match self {
            Self::Reflectivity => "REF",
            Self::Velocity => "VEL",
            Self::Unknown { .. } => "UNK",
        }
    }

    pub fn units(&self) -> &'static str {
        match self {
            Self::Reflectivity => "dBZ",
            Self::Velocity => "m/s",
            Self::Unknown { .. } => "",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmericanVolume {
    pub source_name: Option<String>,
    pub station_id: String,
    pub station_number: Option<u16>,
    pub reference_time: DateTimeParts,
    pub sweeps: Vec<AmericanSweep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmericanSweep {
    pub product: String,
    pub elevation_angle_deg: Option<f32>,
    pub radials: Vec<AmericanRadial>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmericanRadial {
    pub azimuth_deg: f32,
    pub elevation_deg: f32,
    pub moments: Vec<AmericanMoment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmericanMoment {
    pub name: String,
    pub units: String,
    pub first_gate_range_m: f32,
    pub gate_size_m: f32,
    pub data: Vec<f32>,
}
