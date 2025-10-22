use chrono::{NaiveDateTime, NaiveTime};

use nexrad::decode::decode_file;
use nexrad::decompress::decompress_file;
use nexrad::download::{download_file, list_files};
use nexrad::file::is_compressed;
use nexrad::model::DataFile;
use nexrad::result::Result;

/// Check for new radar files without downloading them
/// Returns the timestamp of the latest available scan, or None if no files found
pub async fn check_latest_scan(
    site: &str,
    date: &chrono::NaiveDate,
) -> Result<Option<NaiveDateTime>> {
    let metas = list_files(site, date).await;
    let metas = metas?;

    if metas.is_empty() {
        return Ok(None);
    }

    // Find the latest scan
    let mut latest_time = NaiveTime::from_hms_opt(0, 0, 0).unwrap();
    for m in metas.iter() {
        let identifier_parts = m.identifier().split('_');
        let identifier_time = identifier_parts.collect::<Vec<_>>()[1];
        if let Ok(time) = NaiveTime::parse_from_str(identifier_time, "%H%M%S")
            && time > latest_time
        {
            latest_time = time;
        }
    }

    // Combine date and time
    let latest_datetime = date.and_time(latest_time);
    Ok(Some(latest_datetime))
}

pub async fn get_scan(site: &str, timestamp: NaiveDateTime) -> Result<DataFile> {
    let metas = list_files(site, &timestamp.date()).await;
    let metas = metas?;

    if metas.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "No files found for the specified date.",
        )
        .into());
    }

    println!("Found {} files.", metas.len());

    // Find the closest scan, preferring past scans but accepting future ones
    let mut meta = metas.first().expect("found at least one meta");
    let mut min_diff = i64::MAX;
    let mut latest_meta = meta;
    let mut latest_time = NaiveTime::from_hms_opt(0, 0, 0).unwrap();

    for m in metas.iter() {
        let identifier_parts = m.identifier().split('_');
        let identifier_time = identifier_parts.collect::<Vec<_>>()[1];
        let identifier_time =
            NaiveTime::parse_from_str(identifier_time, "%H%M%S").expect("is valid time");

        // Track the latest available scan
        if identifier_time > latest_time {
            latest_time = identifier_time;
            latest_meta = m;
        }

        let diff = (identifier_time.signed_duration_since(timestamp.time()))
            .num_seconds()
            .abs();

        if diff < min_diff {
            min_diff = diff;
            meta = m;
        }
    }

    // If the closest match is in the future (user requested time is too new),
    // use the latest available scan instead
    let identifier_parts = meta.identifier().split('_');
    let identifier_time = identifier_parts.collect::<Vec<_>>()[1];
    let meta_time = NaiveTime::parse_from_str(identifier_time, "%H%M%S").expect("is valid time");

    if meta_time > timestamp.time() && latest_time < timestamp.time() {
        // Requested time is newer than latest available, use latest
        meta = latest_meta;
        println!("Requested time is too new, using latest available scan.");
    }

    println!(
        "Nearest file to {:?} is {:?}.",
        timestamp.time(),
        meta.identifier()
    );

    println!("Downloading file \"{}\"...", meta.identifier());
    let mut downloaded_file = download_file(meta).await?;

    println!("Data file size (bytes): {}", downloaded_file.len());

    if is_compressed(downloaded_file.as_slice()) {
        downloaded_file = decompress_file(&downloaded_file)?;
    }

    let scan = decode_file(&downloaded_file)?;
    Ok(scan)
}
