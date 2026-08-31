#![cfg(feature = "serial")]

use std::io::BufRead;
use std::sync::mpsc;
use std::time::Duration;

use crate::config::SerialConfig;
use crate::nmea_parser::{NmeaState, ParsedFix};

/// USB VID/PIDs of common GPS chipsets and the USB-serial adapters GPS modules
/// are usually wired through. `None` PID matches every product from that vendor.
const GPS_VID_PIDS: &[(u16, Option<u16>)] = &[
    (0x1546, None),         // u-blox
    (0x067B, None),         // Prolific (PL2303)
    (0x0403, None),         // FTDI
    (0x10C4, None),         // Silicon Labs CP210x
    (0x1A86, None),         // QinHeng CH340
    (0x4292, Some(0x0603)), // SiRF (USB-attached receivers)
];

const COMMON_BAUDS: &[u32] = &[9600, 4800, 38400, 115200];

#[derive(Debug, Clone)]
pub struct GpsPortInfo {
    pub port_name: String,
    pub description: String,
}

pub fn detect_gps_ports() -> Vec<GpsPortInfo> {
    let Ok(ports) = serialport::available_ports() else {
        return Vec::new();
    };

    let mut gps_ports = Vec::new();
    let mut other_ports = Vec::new();

    for port in ports {
        let info = match &port.port_type {
            serialport::SerialPortType::UsbPort(usb) => {
                let is_gps = GPS_VID_PIDS
                    .iter()
                    .any(|(vid, pid)| usb.vid == *vid && pid.is_none_or(|p| usb.pid == p));
                let desc = usb
                    .product
                    .clone()
                    .unwrap_or_else(|| format!("USB {:04X}:{:04X}", usb.vid, usb.pid));
                (
                    GpsPortInfo {
                        port_name: port.port_name.clone(),
                        description: desc,
                    },
                    is_gps,
                )
            }
            serialport::SerialPortType::PciPort => (
                GpsPortInfo {
                    port_name: port.port_name.clone(),
                    description: "PCI serial".to_string(),
                },
                false,
            ),
            serialport::SerialPortType::BluetoothPort => continue,
            serialport::SerialPortType::Unknown => (
                GpsPortInfo {
                    port_name: port.port_name.clone(),
                    description: "Serial port".to_string(),
                },
                false,
            ),
        };
        if info.1 {
            gps_ports.push(info.0);
        } else {
            other_ports.push(info.0);
        }
    }

    gps_ports.extend(other_ports);
    gps_ports
}

/// How stale the port list may get before a fresh scan is asked for.
///
/// A serial port appears when a receiver is plugged in, which is a human act:
/// two seconds is under the time it takes to look back at the screen, and it
/// is 120 frames of not walking the bus at 60 Hz.
pub const RESCAN_INTERVAL: Duration = Duration::from_secs(2);

/// The port list a drawing thread reads — scanned somewhere else.
///
/// [`detect_gps_ports`] walks every tty the platform knows and asks udev about
/// each one: **24.9–25.6 ms** over the 101 ttys of the box this was measured
/// on (2026-08-31, release, native). The settings pane called it inline, once
/// per frame, for as long as the pane was open — so an open inspector cost
/// 25 ms of frame thread per frame and the whole UI stalled at ~35 Hz behind
/// it. This is the same answer with the walk moved off the asking thread.
///
/// `ports` never scans on the caller's thread and never blocks: it answers
/// with the newest finished scan, which is empty until the first one lands a
/// few milliseconds in. That is the product rule — the data may lag, the
/// frame may not — and the combo degrades to the stored port path meanwhile
/// (`gps_port_label`'s fallback), not to a wrong list.
pub struct GpsPortScanner {
    inner: std::sync::Arc<ScanState>,
}

struct ScanState {
    ports: std::sync::Mutex<Vec<GpsPortInfo>>,
    /// When the newest FINISHED scan finished; `None` until the first lands.
    scanned_at: std::sync::Mutex<Option<std::time::Instant>>,
    /// A scan is on its way. Held across the whole thread, so a slow bus
    /// cannot accumulate one scan thread per frame behind it.
    scanning: std::sync::atomic::AtomicBool,
    /// Scans STARTED. The count gate's figure, and started rather than
    /// finished on purpose: a caller counting finished scans is racing the
    /// scan thread, and a race in a gate is a flaky gate.
    started: std::sync::atomic::AtomicUsize,
    /// Scans FINISHED. The freshness half — what says a list has landed.
    scans: std::sync::atomic::AtomicUsize,
}

impl Default for GpsPortScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl GpsPortScanner {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(ScanState {
                ports: std::sync::Mutex::new(Vec::new()),
                scanned_at: std::sync::Mutex::new(None),
                scanning: std::sync::atomic::AtomicBool::new(false),
                started: std::sync::atomic::AtomicUsize::new(0),
                scans: std::sync::atomic::AtomicUsize::new(0),
            }),
        }
    }

    /// The newest finished scan's answer, plus a request for a fresher one if
    /// the newest has aged out. Never scans here.
    pub fn ports(&self) -> Vec<GpsPortInfo> {
        self.start_due_scan(std::time::Instant::now(), detect_gps_ports);
        lock(&self.inner.ports).clone()
    }

    /// Scans that have STARTED — how many times this scanner has walked the
    /// bus. A level a test reads; nothing in the app branches on it.
    pub fn scans_started(&self) -> usize {
        self.inner
            .started
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Scans that have FINISHED, so a caller can tell a list has landed.
    pub fn scans(&self) -> usize {
        self.inner.scans.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Start a scan if one is due and none is running, and say whether one
    /// started. The seam the count gate drives, with `now` and the scan
    /// itself passed in so the gate needs neither a clock nor a serial bus.
    fn start_due_scan(&self, now: std::time::Instant, scan: fn() -> Vec<GpsPortInfo>) -> bool {
        use std::sync::atomic::Ordering;
        let fresh = lock(&self.inner.scanned_at)
            .is_some_and(|at| now.saturating_duration_since(at) < RESCAN_INTERVAL);
        if fresh {
            return false;
        }
        // The claim and the test are one operation: two frames landing here
        // together must start one scan between them, not two.
        if self.inner.scanning.swap(true, Ordering::AcqRel) {
            return false;
        }
        self.inner.started.fetch_add(1, Ordering::Relaxed);
        let inner = std::sync::Arc::clone(&self.inner);
        std::thread::spawn(move || {
            let found = scan();
            *lock(&inner.ports) = found;
            *lock(&inner.scanned_at) = Some(std::time::Instant::now());
            inner.scans.fetch_add(1, Ordering::Relaxed);
            inner.scanning.store(false, Ordering::Release);
        });
        true
    }
}

/// A poisoned lock here means a scan thread panicked mid-write. The list it
/// was writing is a list of serial ports; the honest recovery is to carry on
/// with whatever is in there rather than to take the UI down with it.
fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod scanner_tests {
    use super::{GpsPortInfo, GpsPortScanner, RESCAN_INTERVAL};
    use std::time::{Duration, Instant};

    fn no_ports() -> Vec<GpsPortInfo> {
        Vec::new()
    }

    /// **Four seconds of frames start one scan, not two hundred and forty.**
    ///
    /// The shape this replaces called the bus walk once per ask, which is what
    /// the settings pane's per-frame call made into 25 ms of frame thread
    /// every frame. Red against that shape — and red against dropping either
    /// guard: without the freshness test 240 start, without the in-flight test
    /// a slow scan lets the next ask start another.
    #[test]
    fn many_asks_inside_the_interval_start_one_scan() {
        let scanner = GpsPortScanner::new();
        let now = Instant::now();
        let started = (0..240)
            .filter(|_| scanner.start_due_scan(now, no_ports))
            .count();
        assert_eq!(
            started, 1,
            "240 frames of an open settings pane started {started} bus walks; \
             one is the whole point of the scanner",
        );
    }

    /// And the list is not frozen: once the newest scan has aged out, the next
    /// ask starts another. The non-triviality floor under the test above — a
    /// scanner that never scans would pass that one.
    #[test]
    fn an_ask_after_the_interval_starts_another_scan() {
        let scanner = GpsPortScanner::new();
        let now = Instant::now();
        assert!(
            scanner.start_due_scan(now, no_ports),
            "fixture: the first ask must start the first scan",
        );
        // The first scan has to have LANDED before it can be stale; it is a
        // thread, and it stamps its own finish.
        let deadline = Instant::now() + Duration::from_secs(5);
        while scanner.scans() == 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(scanner.scans(), 1, "fixture: the first scan finished");
        assert!(
            !scanner.start_due_scan(Instant::now(), no_ports),
            "an ask inside the interval must not start a scan",
        );
        assert!(
            scanner.start_due_scan(
                Instant::now() + RESCAN_INTERVAL + Duration::from_millis(500),
                no_ports,
            ),
            "an ask past the interval must start a fresh scan, or the list a \
             plugged-in receiver should appear in never changes",
        );
    }
}

/// Probe each rate in [`COMMON_BAUDS`] for a line starting with `$`. Worst case
/// is tens of seconds of blocking reads, so it runs on the reader thread and
/// checks the stop signal between reads.
fn detect_baud(port_name: &str, stop_rx: &mpsc::Receiver<()>) -> Option<u32> {
    for &baud in COMMON_BAUDS {
        let port = serialport::new(port_name, baud)
            .timeout(Duration::from_millis(1500))
            .open();
        let Ok(port) = port else { continue };

        let mut reader = std::io::BufReader::new(port);
        let mut buf = String::new();

        for _ in 0..5 {
            if should_stop(stop_rx) {
                return None;
            }
            buf.clear();
            if reader.read_line(&mut buf).is_ok() && buf.starts_with('$') {
                return Some(baud);
            }
        }
    }
    None
}

pub struct SerialGpsReader {
    _stop_signal: mpsc::Sender<()>,
}

impl SerialGpsReader {
    /// Spawns the reader thread and returns it immediately; port and baud
    /// auto-detection happens on that thread, because probing can block for
    /// tens of seconds and the caller is the GUI thread.
    ///
    /// `on_fix` is called on the reader thread with every parsed
    /// position-bearing sentence. Returning `false` stops the thread — which
    /// holds the port open exclusively, so a consumer that vanishes without
    /// stopping the reader would pin the device forever.
    pub fn start(
        config: &SerialConfig,
        on_fix: impl FnMut(ParsedFix) -> bool + Send + 'static,
    ) -> Option<Self> {
        let configured_port = config.port_path.clone();
        let auto_baud = config.auto_baud();
        let configured_baud = config.baud_rate;

        let (stop_tx, stop_rx) = mpsc::channel();

        std::thread::Builder::new()
            .name("gps-serial".into())
            .spawn(move || {
                let Some(port_name) = resolve_port(configured_port, &stop_rx) else {
                    return;
                };
                let baud = if auto_baud {
                    detect_baud(&port_name, &stop_rx).unwrap_or(9600)
                } else {
                    configured_baud
                };
                log::info!("Starting GPS reader on {} @ {} baud", port_name, baud);
                let mut on_fix = on_fix;
                gps_read_loop(&port_name, baud, &stop_rx, &mut on_fix);
            })
            .expect("failed to spawn gps-serial thread");

        Some(Self {
            _stop_signal: stop_tx,
        })
    }
}

/// The configured port when set, otherwise the first detected port — retrying
/// every 5s so a receiver plugged in later is still picked up.
fn resolve_port(configured: Option<String>, stop_rx: &mpsc::Receiver<()>) -> Option<String> {
    if let Some(path) = configured {
        return Some(path);
    }
    loop {
        if let Some(port) = detect_gps_ports().first() {
            return Some(port.port_name.clone());
        }
        log::warn!("No GPS port found. Retrying detection in 5s");
        for _ in 0..50 {
            if should_stop(stop_rx) {
                return None;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Whether the reader thread must exit: an explicit stop `()` arrived, or the
/// [`SerialGpsReader`] holding the sender was dropped. A bare
/// `try_recv().is_ok()` would read the drop as "keep going" and leak the
/// thread, which holds the port open exclusively.
fn should_stop(stop_rx: &mpsc::Receiver<()>) -> bool {
    !matches!(stop_rx.try_recv(), Err(mpsc::TryRecvError::Empty))
}

fn gps_read_loop(
    port_name: &str,
    baud: u32,
    stop_rx: &mpsc::Receiver<()>,
    on_fix: &mut impl FnMut(ParsedFix) -> bool,
) {
    loop {
        if should_stop(stop_rx) {
            log::info!("GPS reader stopping (signal received)");
            return;
        }

        let port = serialport::new(port_name, baud)
            .timeout(Duration::from_millis(2000))
            .open();

        let port = match port {
            Ok(p) => {
                log::info!("GPS serial port opened: {} @ {}", port_name, baud);
                p
            }
            Err(e) => {
                log::warn!(
                    "Failed to open GPS port {}: {}. Retrying in 5s",
                    port_name,
                    e
                );
                for _ in 0..50 {
                    if should_stop(stop_rx) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                continue;
            }
        };

        let mut reader = std::io::BufReader::new(port);
        let mut nmea = NmeaState::new();
        let mut line = String::new();

        loop {
            if should_stop(stop_rx) {
                log::info!("GPS reader stopping (signal received)");
                return;
            }

            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    log::warn!("GPS port EOF, reconnecting in 5s");
                    break;
                }
                Ok(_) => {
                    let trimmed = line.trim();
                    if let Some(fix) = nmea.feed_sentence(trimmed)
                        && !on_fix(fix)
                    {
                        log::info!("GPS fix consumer gone, stopping reader");
                        return;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                    // A quiet receiver, not a disconnect: do not reconnect.
                    continue;
                }
                Err(e) => {
                    log::warn!("GPS read error: {}. Reconnecting in 5s", e);
                    break;
                }
            }
        }

        for _ in 0..50 {
            if should_stop(stop_rx) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_stop_stays_false_while_the_sender_is_alive_and_silent() {
        let (_stop_tx, stop_rx) = mpsc::channel::<()>();
        assert!(!should_stop(&stop_rx));
    }

    #[test]
    fn should_stop_fires_on_an_explicit_stop_message() {
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        stop_tx.send(()).unwrap();
        assert!(should_stop(&stop_rx));
    }

    /// Dropping [`SerialGpsReader`] drops the sender without ever sending, so
    /// the thread must read `Disconnected` as stop.
    #[test]
    fn should_stop_fires_when_the_sender_is_dropped_without_sending() {
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        drop(stop_tx);
        assert!(should_stop(&stop_rx));
    }
}
