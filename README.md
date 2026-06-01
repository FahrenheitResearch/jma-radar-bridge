# jma-radar-rs

Pure Rust decoder for JMA radar-per-site polar-coordinate GRIB2 files, including
the local JMA templates used by the NICT archive:

- grid definition template `3.50120`
- product definition template `4.51022`
- run-length data representation template `5.200`

The library returns a normalized radial volume with NEXRAD-style product names
(`REF`, `VEL`) so American radar tooling can work from a familiar sweep/ray/gate
shape. The included CLI renders quick-look PNGs from either a single `.bin`
GRIB2 file or a NICT `.tar` bundle.

## Build

```bash
cargo build --release
```

## Render Proof PNGs

```bash
cargo run --release --bin jma-radar-render -- \
  path/to/Z__C_RJTD_yyyyMMddhhmmss_RDR_JMAGPV_N6_grib2.tar \
  --out-dir proof \
  --max-images 6
```

The output PNGs are quick-look PPI images. They are intended as decoder proof
and QA, not as a final scientific plotting style.

## Library Sketch

```rust
let bytes = std::fs::read("radar.bin")?;
let volume = jma_radar_rs::decode_grib2(&bytes, Some("radar.bin"))?;

for sweep in &volume.sweeps {
    println!(
        "{} elev {:.2} gates {} rays {}",
        sweep.product.short_name(),
        sweep.elevation_angle_deg.unwrap_or(f32::NAN),
        sweep.grid.gate_count,
        sweep.grid.radial_count,
    );
}
```

## Notes

This crate intentionally does not pretend to emit valid WSR-88D Archive II
Level II yet. It produces a clean radial model suitable for an Archive II writer,
RustDar adapters, CF/Radial writers, or PNG/GeoTIFF style downstream exporters.
