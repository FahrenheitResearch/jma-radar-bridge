use std::fs;
use std::fs::File;
use std::io::Read;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use jma_radar_bridge::{
    decode_input_file, download_latest, publish_bytes, publish_volumes, BridgeOptions, NictProduct,
    Product, Volume,
};
use tiny_http::{Header, Request, Response, Server, StatusCode};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Parser)]
#[command(
    name = "jma-radar",
    about = "Decode JMA polar radar GRIB2 and serve easy proof products"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Render a one-shot bridge proof bundle from a JMA .tar or .bin.
    Render(RenderArgs),
    /// Serve a local GRLevel2/GR2Analyst-style polling directory.
    Live(LiveArgs),
    /// Check that a bridge polling URL is reachable and has the expected files.
    Doctor(DoctorArgs),
    /// Print a compact text summary of a JMA .tar or .bin.
    Inspect(InspectArgs),
}

#[derive(Debug, Args)]
struct RenderArgs {
    input: PathBuf,
    #[arg(long, default_value = "proof")]
    out_dir: PathBuf,
    #[arg(long)]
    site: Option<String>,
    #[arg(long, default_value_t = 1200)]
    size: u32,
    #[arg(long, default_value_t = 8)]
    max_images: usize,
}

#[derive(Debug, Args)]
struct InspectArgs {
    input: PathBuf,
}

#[derive(Debug, Args)]
struct DoctorArgs {
    #[arg(default_value = "http://127.0.0.1:8787/level2/raw/")]
    polling_root: String,
}

#[derive(Debug, Args, Clone)]
struct LiveArgs {
    /// Decode this fixed file before serving.
    #[arg(long)]
    input: Option<PathBuf>,
    /// Re-publish the newest .tar/.bin in this folder every interval.
    #[arg(long)]
    watch_dir: Option<PathBuf>,
    /// Re-fetch and publish this exact JMA tar/bin URL every interval.
    #[arg(long)]
    url: Option<String>,
    /// Use the current NICT JMA live feed. This is the default when no source is given.
    #[arg(long)]
    nict_latest: bool,
    /// NICT products to fetch when using --nict-latest or no source.
    #[arg(long, value_enum, default_value_t = NictSet::Both)]
    nict_products: NictSet,
    /// Minutes of recent NICT file names to try.
    #[arg(long, default_value_t = 180)]
    nict_lookback_minutes: i64,
    /// Local HTTP bind address.
    #[arg(long, default_value = "127.0.0.1:8787")]
    bind: SocketAddr,
    /// Directory that will contain level2/raw.
    #[arg(long, default_value = "bridge-out")]
    out_dir: PathBuf,
    /// Re-poll interval for live sources.
    #[arg(long, default_value_t = 60)]
    interval_secs: u64,
    /// Override the 4-character served radar id.
    #[arg(long)]
    site: Option<String>,
    #[arg(long, default_value_t = 900)]
    size: u32,
    #[arg(long, default_value_t = 8)]
    max_images: usize,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum NictSet {
    N5,
    N6,
    Both,
}

impl NictSet {
    fn products(self) -> &'static [NictProduct] {
        match self {
            Self::N5 => &[NictProduct::N5],
            Self::N6 => &[NictProduct::N6],
            Self::Both => &[NictProduct::N5, NictProduct::N6],
        }
    }
}

#[derive(Debug, Clone)]
enum LiveSource {
    Input(PathBuf),
    WatchDir(PathBuf),
    Url(String),
    NictLatest {
        products: NictSet,
        lookback_minutes: i64,
    },
}

fn main() -> Result<(), BoxError> {
    let cli = Cli::parse();
    match cli.command {
        Command::Render(args) => render(args),
        Command::Live(args) => live(args),
        Command::Doctor(args) => doctor(args),
        Command::Inspect(args) => inspect(args),
    }
}

fn render(args: RenderArgs) -> Result<(), BoxError> {
    let volumes = decode_input_file(&args.input)?;
    let report = publish_volumes(
        &volumes,
        &BridgeOptions {
            raw_root: args.out_dir,
            site_override: args.site,
            render_size_px: args.size,
            max_images: args.max_images,
        },
    )?;
    eprintln!(
        "rendered {} proof PNG(s) for {} to {}",
        report.proof_files.len(),
        report.site_id,
        report.station_dir.display()
    );
    Ok(())
}

fn inspect(args: InspectArgs) -> Result<(), BoxError> {
    let volumes = decode_input_file(&args.input)?;
    for volume in volumes {
        println!(
            "{}  station={}  sweeps={}",
            volume.identification.reference_time.iso_utc(),
            volume.station_id().unwrap_or("unknown"),
            volume.sweeps.len()
        );
        for (idx, sweep) in volume.sweeps.iter().enumerate() {
            println!(
                "  sweep {idx:02} {:>3} elev={:>5.2} deg gates={} rays={} range={:.1} km non-missing={}",
                product_name(&sweep.product),
                sweep.elevation_angle_deg.unwrap_or(0.0),
                sweep.grid.gate_count,
                sweep.grid.radial_count,
                sweep.grid.max_range_m() / 1000.0,
                sweep.non_nan_count()
            );
        }
    }
    Ok(())
}

fn doctor(args: DoctorArgs) -> Result<(), BoxError> {
    let root = args.polling_root.trim_end_matches('/').to_string();
    fetch_text(&format!("{root}/grlevel2.cfg"))?;
    println!("ok grlevel2.cfg");

    fetch_text(&format!("{root}/customradars.gis"))?;
    println!("ok customradars.gis");

    let cfg = fetch_text(&format!("{root}/grlevel2.cfg"))?;
    let site_id = cfg
        .lines()
        .find_map(|line| line.strip_prefix("Site:"))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("JMA1");
    fetch_text(&format!("{root}/{site_id}/latest.json"))?;
    println!("ok {site_id}/latest.json");
    println!("polling root looks alive: {root}/");
    Ok(())
}

fn live(args: LiveArgs) -> Result<(), BoxError> {
    let source = live_source(&args);
    let public_root = args.out_dir.clone();
    let options = BridgeOptions {
        raw_root: public_root.join("level2").join("raw"),
        site_override: args.site.clone(),
        render_size_px: args.size,
        max_images: args.max_images,
    };
    fs::create_dir_all(&options.raw_root)?;

    publish_from_source(&source, &options)?;

    if !matches!(source, LiveSource::Input(_)) {
        let updater_source = source.clone();
        let updater_options = options.clone();
        let interval = Duration::from_secs(args.interval_secs.max(5));
        thread::spawn(move || loop {
            thread::sleep(interval);
            if let Err(err) = publish_from_source(&updater_source, &updater_options) {
                eprintln!("live update failed: {err}");
            }
        });
    }

    println!("Polling root: http://{}/level2/raw/", args.bind);
    println!(
        "Custom radar file: http://{}/level2/raw/customradars.gis",
        args.bind
    );
    serve_directory(public_root, args.bind)
}

fn live_source(args: &LiveArgs) -> LiveSource {
    if let Some(path) = &args.input {
        LiveSource::Input(path.clone())
    } else if let Some(path) = &args.watch_dir {
        LiveSource::WatchDir(path.clone())
    } else if let Some(url) = &args.url {
        LiveSource::Url(url.clone())
    } else {
        LiveSource::NictLatest {
            products: args.nict_products,
            lookback_minutes: args.nict_lookback_minutes,
        }
    }
}

fn publish_from_source(source: &LiveSource, options: &BridgeOptions) -> Result<(), BoxError> {
    let report = match source {
        LiveSource::Input(path) => {
            let bytes = fs::read(path)?;
            publish_bytes(&bytes, path.file_name().and_then(|s| s.to_str()), options)?
        }
        LiveSource::WatchDir(path) => {
            let latest = latest_input(path)
                .ok_or_else(|| format!("no .tar, .bin, or .grib2 files in {}", path.display()))?;
            let bytes = fs::read(&latest)?;
            publish_bytes(&bytes, latest.file_name().and_then(|s| s.to_str()), options)?
        }
        LiveSource::Url(url) => {
            let bytes = fetch_url(url)?;
            let source_name = url.rsplit('/').next();
            publish_bytes(&bytes, source_name, options)?
        }
        LiveSource::NictLatest {
            products,
            lookback_minutes,
        } => {
            let downloads = download_latest(products.products(), *lookback_minutes)?;
            let mut volumes = Vec::<Volume>::new();
            for download in downloads {
                eprintln!("downloaded {}", download.url);
                let mut decoded = jma_radar_bridge::decode_input_bytes(
                    &download.bytes,
                    Some(&download.file_name),
                )?;
                volumes.append(&mut decoded);
            }
            publish_volumes(&volumes, options)?
        }
    };

    eprintln!(
        "published {} sweeps for {} in {}",
        report.sweep_count,
        report.site_id,
        report.raw_root.display()
    );
    Ok(())
}

fn latest_input(dir: &Path) -> Option<PathBuf> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("tar")
                        || ext.eq_ignore_ascii_case("bin")
                        || ext.eq_ignore_ascii_case("grib2")
                })
        })
        .collect();
    entries.sort_by_key(|entry| {
        entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
    });
    entries.pop().map(|entry| entry.path())
}

fn fetch_url(url: &str) -> Result<Vec<u8>, BoxError> {
    let response = ureq::get(url).call()?;
    let mut reader = response.into_reader();
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn fetch_text(url: &str) -> Result<String, BoxError> {
    let bytes = fetch_url(url)?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

fn serve_directory(public_root: PathBuf, bind: SocketAddr) -> Result<(), BoxError> {
    let server = Server::http(bind)?;
    for request in server.incoming_requests() {
        handle_request(request, &public_root);
    }
    Ok(())
}

fn handle_request(request: Request, public_root: &Path) {
    match request_path(public_root, request.url()) {
        Ok(path) if path.is_file() => respond_file(request, &path),
        Ok(path) if path.is_dir() => respond_file(request, &path.join("index.html")),
        Ok(_) => {
            let response = Response::from_string("not found").with_status_code(StatusCode(404));
            let _ = request.respond(response);
        }
        Err(message) => {
            let response = Response::from_string(message).with_status_code(StatusCode(400));
            let _ = request.respond(response);
        }
    }
}

fn request_path(public_root: &Path, url: &str) -> Result<PathBuf, String> {
    let path = url.split('?').next().unwrap_or("/");
    let rel = match path {
        "/" => "level2/raw/index.html",
        "/level2/raw" | "/level2/raw/" => "level2/raw/index.html",
        "/customradars.gis" => "level2/raw/customradars.gis",
        other => other.trim_start_matches('/'),
    };

    if rel
        .split('/')
        .any(|part| part == ".." || part.contains('\\') || part.contains(':'))
    {
        return Err("invalid path".to_string());
    }

    Ok(public_root.join(rel))
}

fn respond_file(request: Request, path: &Path) {
    match File::open(path) {
        Ok(file) => {
            let mut response = Response::from_file(file);
            if let Some(header) = content_type_header(path) {
                response.add_header(header);
            }
            let _ = request.respond(response);
        }
        Err(_) => {
            let response = Response::from_string("not found").with_status_code(StatusCode(404));
            let _ = request.respond(response);
        }
    }
}

fn content_type_header(path: &Path) -> Option<Header> {
    let mime = match path.extension().and_then(|s| s.to_str()).unwrap_or("") {
        "cfg" | "gis" | "txt" | "list" => "text/plain; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "json" => "application/json",
        "png" => "image/png",
        _ => "application/octet-stream",
    };
    Header::from_bytes("Content-Type", mime).ok()
}

fn product_name(product: &Product) -> &'static str {
    match product {
        Product::Reflectivity => "REF",
        Product::Velocity => "VEL",
        Product::Unknown { .. } => "UNK",
    }
}
