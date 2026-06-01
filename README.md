# jma-radar-bridge

`jma-radar-bridge` is a pure Rust decoder and local bridge for Japan
Meteorological Agency polar-coordinate radar GRIB2 files.

It is meant for people who want JMA radar data in a friendlier American-radar
shape: sweeps, radials, gates, `REF`, `VEL`, quick-look PNGs, JSON metadata,
and a local polling directory that looks like a GRLevel2/GR2Analyst Level II
feed.

Important status: the bridge serves the polling URL layout now. Native NEXRAD
Archive II output, the part GR2Analyst needs for direct ingest, is the next
writer to add.

## Install

```bash
cargo install --git https://github.com/FahrenheitResearch/jma-radar-bridge
```

Or build from a checkout:

```bash
cargo build --release
```

## Start a Local Polling Bridge

For the current NICT/JMA feed:

```bash
jma-radar live
```

That serves:

```text
http://127.0.0.1:8787/level2/raw/
http://127.0.0.1:8787/level2/raw/grlevel2.cfg
http://127.0.0.1:8787/level2/raw/customradars.gis
```

For a downloaded NICT tar:

```bash
jma-radar live --input Z__C_RJTD_20260601000000_RDR_JMAGPV_N5_grib2.tar
```

For a folder where another downloader drops files:

```bash
jma-radar live --watch-dir ./incoming
```

Check the served bridge:

```bash
jma-radar doctor http://127.0.0.1:8787/level2/raw/
```

## Make Proof PNGs

```bash
jma-radar render Z__C_RJTD_20260601000000_RDR_JMAGPV_N6_grib2.tar --out-dir proof
```

The command writes a small bridge bundle:

```text
proof/
  grlevel2.cfg
  customradars.gis
  ITOK/
    latest.json
    *.png
```

## Inspect a File

```bash
jma-radar inspect Z__C_RJTD_20260601000000_RDR_JMAGPV_N5_grib2.tar
```

## Rust API

```rust
let bytes = std::fs::read("radar.tar")?;
let volumes = jma_radar_bridge::decode_input_bytes(&bytes, Some("radar.tar"))?;

for volume in volumes {
    let american = volume.to_american_volume();
    println!("{} sweeps", american.sweeps.len());
}
```

## Decoder Coverage

The decoder currently supports the JMA local templates used by the NICT
per-radar polar archive:

- Grid definition template `3.50120`
- Product definition template `4.51022`
- Run-length data representation template `5.200`
- Reflectivity mapped to `REF`
- Doppler velocity mapped to `VEL`

The run-length reader handles signed-magnitude representative values, which is
needed for velocity.

## GR2Analyst Plan

The user flow should eventually be:

```text
jma-radar live
paste http://127.0.0.1:8787/level2/raw/ into GR2Analyst polling
```

The current bridge already provides the polling shape and radar metadata files.
The remaining compatibility work is a real Archive II writer from the normalized
radial model.

## Data Sources

- NICT JMA polar radar archive: https://pawr.nict.go.jp/jmadata/JMA-PolarCoordsRadar/
- JMA format specification no13702: https://www.data.jma.go.jp/suishin/shiyou/pdf/no13702
