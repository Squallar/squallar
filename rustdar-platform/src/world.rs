use chrono::NaiveDateTime;
use nexrad::model::DataFile;
use rustdar_egui::ScanInfo;

pub struct World {
    scan_data: Option<DataFile>,
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

impl World {
    pub fn new() -> Self {
        Self { scan_data: None }
    }

    pub fn draw(&mut self, _frame: &mut [u8]) {
        // TODO: Implement actual rendering
    }

    pub fn update(&mut self) {
        // TODO: Implement world update logic
    }

    /// Load scan data from the fetched radar data
    pub fn load_scan_data(
        &mut self,
        data: DataFile,
        site: &str,
        _requested_timestamp: NaiveDateTime,
    ) -> ScanInfo {
        let num_elevations = data.elevation_scans().len();

        // Extract actual timestamp from the volume header
        let volume_header = data.volume_header();
        let file_date = volume_header.file_date(); // Days since January 1, 1970 (day 1 = Jan 1, 1970)
        let file_time = volume_header.file_time(); // Milliseconds since midnight (UTC)

        // Convert days since January 1, 1970 to NaiveDate
        // Note: The NEXRAD format uses day 1 = Jan 1, 1970, so we subtract 1
        let unix_epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
        let scan_date = unix_epoch + chrono::Duration::days((file_date - 1) as i64);

        // Convert milliseconds since midnight to time
        let total_seconds = file_time / 1000;
        let hours = total_seconds / 3600;
        let minutes = (total_seconds % 3600) / 60;
        let seconds = total_seconds % 60;
        let millis = file_time % 1000;

        let scan_time = chrono::NaiveTime::from_hms_milli_opt(
            hours as u32,
            minutes as u32,
            seconds as u32,
            millis as u32,
        )
        .unwrap_or_else(|| chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap());

        // This timestamp is in UTC
        let actual_timestamp = chrono::NaiveDateTime::new(scan_date, scan_time);

        self.scan_data = Some(data);

        ScanInfo {
            site: site.to_string(),
            timestamp: actual_timestamp,
            num_elevations,
            status: format!("Loaded {} elevation angles", num_elevations),
        }
    }

    /// Get a reference to the current scan data
    pub fn scan_data(&self) -> Option<&DataFile> {
        self.scan_data.as_ref()
    }
}
