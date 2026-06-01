use std::io::Read;

use thiserror::Error;
use time::OffsetDateTime;

const BASE_URL: &str = "https://pawr.nict.go.jp/jmadata/JMA-PolarCoordsRadar";

#[derive(Debug, Clone, Copy)]
pub enum NictProduct {
    N5,
    N6,
}

impl NictProduct {
    pub fn code(self) -> &'static str {
        match self {
            Self::N5 => "N5",
            Self::N6 => "N6",
        }
    }
}

#[derive(Debug)]
pub struct NictDownload {
    pub url: String,
    pub file_name: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum NictError {
    #[error("no recent NICT tar found for {product} in the last {lookback_minutes} minutes")]
    NotFound {
        product: &'static str,
        lookback_minutes: i64,
    },
    #[error("HTTP error while fetching {url}: {source}")]
    Http {
        url: String,
        #[source]
        source: Box<ureq::Error>,
    },
    #[error("I/O error while reading {url}: {source}")]
    Io {
        url: String,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid system time")]
    Time(#[from] time::error::ComponentRange),
}

pub fn download_latest(
    products: &[NictProduct],
    lookback_minutes: i64,
) -> Result<Vec<NictDownload>, NictError> {
    let mut downloads = Vec::new();
    let mut last_error = None;
    for product in products {
        match download_latest_product(*product, lookback_minutes) {
            Ok(download) => downloads.push(download),
            Err(err) => last_error = Some(err),
        }
    }
    if downloads.is_empty() {
        return Err(last_error.unwrap_or(NictError::NotFound {
            product: "none",
            lookback_minutes,
        }));
    }
    Ok(downloads)
}

fn download_latest_product(
    product: NictProduct,
    lookback_minutes: i64,
) -> Result<NictDownload, NictError> {
    for url in candidate_urls(product, lookback_minutes) {
        match fetch(&url) {
            Ok(bytes) => {
                let file_name = url.rsplit('/').next().unwrap_or("jma.tar").to_string();
                return Ok(NictDownload {
                    url,
                    file_name,
                    bytes,
                });
            }
            Err(_) => continue,
        }
    }

    Err(NictError::NotFound {
        product: product.code(),
        lookback_minutes,
    })
}

fn fetch(url: &str) -> Result<Vec<u8>, NictError> {
    let response = ureq::get(url).call().map_err(|source| NictError::Http {
        url: url.to_string(),
        source: Box::new(source),
    })?;
    let mut reader = response.into_reader();
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|source| NictError::Io {
            url: url.to_string(),
            source,
        })?;
    Ok(bytes)
}

fn candidate_urls(product: NictProduct, lookback_minutes: i64) -> Vec<String> {
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let mut urls = Vec::new();
    let lookback = lookback_minutes.max(0);
    let mut seen = std::collections::BTreeSet::new();

    for minute_offset in 0..=lookback {
        let ts = now - minute_offset * 60;
        let rounded = ts - ts.rem_euclid(300);
        if !seen.insert(rounded) {
            continue;
        }
        if let Ok(dt) = OffsetDateTime::from_unix_timestamp(rounded) {
            urls.push(nict_url(product, dt));
        }
    }

    urls
}

fn nict_url(product: NictProduct, dt: OffsetDateTime) -> String {
    let year = dt.year();
    let month = u8::from(dt.month());
    let day = dt.day();
    let hour = dt.hour();
    let minute = dt.minute();
    let stamp = format!("{year:04}{month:02}{day:02}{hour:02}{minute:02}00");
    let file_name = format!("Z__C_RJTD_{stamp}_RDR_JMAGPV_{}_grib2.tar", product.code());
    format!("{BASE_URL}/{year:04}/{month:02}/{day:02}/{file_name}")
}
