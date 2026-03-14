use chrono::{NaiveDateTime};
use crate::channels::{ScanResponse, ScanData, Level3Response};
use rustdar_radar::types::RadarProduct;
use rustdar_radar::scan;

impl super::App {

    /// Spawn an async radar data fetch on the background runtime.
    /// Handles generation tracking, result sending, and redraw requests.
    pub fn spawn_fetch(&mut self, site: String, timestamp: NaiveDateTime) {
        let generation = self.render.next_fetch_generation();
        let window = self.window.clone();
        let sender = self.channels.scan_sender.clone();
        self.tokio_runtime.spawn(async move {
            log::info!("Fetching {} @ {} UTC", site, timestamp);
            let msg = match scan::get_scan(&site, timestamp).await {
                Ok(data) => {
                    log::info!("Fetched scan: {} @ {}", site, timestamp);
                    Ok(ScanData { scan: data, site, timestamp })
                }
                Err(e) => {
                    let err = format!("Failed to fetch radar scan: {:?}", e);
                    log::error!("{}", err);
                    Err(err)
                }
            };
            let _ = sender.send(ScanResponse { generation, result: msg });
            if let Some(w) = window { w.request_redraw(); }
        });
    }

    /// Spawn Level III product fetches for all supported Level III products.
    /// Called after a Level II scan loads so the products are available
    /// alongside the base moments.
    pub(super) fn spawn_level3_fetches(&self, site: &str, _timestamp: NaiveDateTime) {
        let generation = self.render.fetch_generation;
        for l3_product in RadarProduct::all().iter().filter(|p| p.is_level3()) {
            let Some(dirs) = l3_product.tgftp_dirs() else { continue };
            for &dir in dirs {
                let site = site.to_string();
                let dir_str = dir.to_string();
                let product = *l3_product;
                let sender = self.channels.level3_sender.clone();
                let window = self.window.clone();
                self.tokio_runtime.spawn(async move {
                    log::info!("Fetching TGFTP {} for {}", dir_str, site);
                    let result = match scan::get_tgftp_product(&site, &dir_str).await {
                        Ok(msg) => {
                            log::info!("Fetched TGFTP {} for {}", dir_str, site);
                            Ok(msg)
                        }
                        Err(e) => {
                            log::warn!("TGFTP {} fetch failed: {}", dir_str, e);
                            Err(format!("{e}"))
                        }
                    };
                    let _ = sender.send(Level3Response { generation, product, tilt_code: dir_str, result });
                    if let Some(w) = window { w.request_redraw(); }
                });
            }
        }
    }
}
