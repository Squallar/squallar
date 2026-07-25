#![cfg(feature = "serial")]

use std::io::BufRead;
use std::sync::mpsc;
use std::time::Duration;

use crate::config::GpsConfig;
use crate::nmea_parser::NmeaState;
use crate::types::GpsFix;

/// Known USB vendor/product IDs for common GPS receiver chipsets and
/// USB-to-serial adapters frequently used with GPS modules.
const GPS_VID_PIDS: &[(u16, Option<u16>)] = &[
    (0x1546, None),        // u-blox
    (0x067B, None),        // Prolific (PL2303)
    (0x0403, None),        // FTDI
    (0x10C4, None),        // Silicon Labs CP210x
    (0x1A86, None),        // QinHeng CH340
    (0x4292, Some(0x0603)), // SiRF (USB-attached receivers)
];

/// Common baud rates for GPS devices, ordered by likelihood.
const COMMON_BAUDS: &[u32] = &[9600, 4800, 38400, 115200];

/// Information about a detected serial port that may be a GPS device.
#[derive(Debug, Clone)]
pub struct GpsPortInfo {
    pub port_name: String,
    pub description: String,
}

/// Scan for serial ports that are likely GPS receivers.
///
/// First returns ports matching known GPS USB VID/PIDs, then all other
/// serial ports. The caller can present these to the user for selection.
pub fn detect_gps_ports() -> Vec<GpsPortInfo> {
    let Ok(ports) = serialport::available_ports() else {
        return Vec::new();
    };

    let mut gps_ports = Vec::new();
    let mut other_ports = Vec::new();

    for port in ports {
        let info = match &port.port_type {
            serialport::SerialPortType::UsbPort(usb) => {
                let is_gps = GPS_VID_PIDS.iter().any(|(vid, pid)| {
                    usb.vid == *vid && pid.is_none_or(|p| usb.pid == p)
                });
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

/// Try to detect the baud rate of a GPS device on a given port.
///
/// Opens the port at each common rate, reads for a short window, and checks
/// if any valid NMEA sentences (`$` prefix) appear.
fn detect_baud(port_name: &str) -> Option<u32> {
    for &baud in COMMON_BAUDS {
        let port = serialport::new(port_name, baud)
            .timeout(Duration::from_millis(1500))
            .open();
        let Ok(port) = port else { continue };

        let mut reader = std::io::BufReader::new(port);
        let mut buf = String::new();

        // Read a few lines and check for NMEA `$` prefix
        for _ in 0..5 {
            buf.clear();
            if reader.read_line(&mut buf).is_ok() && buf.starts_with('$') {
                return Some(baud);
            }
        }
    }
    None
}

/// Manages a serial GPS connection on a background thread.
///
/// Spawns a reader thread that opens the serial port, reads NMEA sentences,
/// and sends [`GpsFix`] updates through the provided channel. Handles
/// disconnection with automatic reconnect attempts.
pub struct SerialGpsReader {
    /// Dropping the sender signals the reader thread to stop.
    _stop_signal: mpsc::Sender<()>,
}

impl SerialGpsReader {
    /// Start reading GPS data from a serial port.
    ///
    /// If `config.port_path` is `None`, auto-detects the port.
    /// If `config.baud_rate` is 0, auto-detects the baud rate.
    ///
    /// Returns `None` if no suitable port could be found.
    pub fn start(config: &GpsConfig, fix_sender: mpsc::Sender<GpsFix>) -> Option<Self> {
        let port_name = if let Some(ref path) = config.port_path {
            path.clone()
        } else {
            let ports = detect_gps_ports();
            ports.first()?.port_name.clone()
        };

        let baud = if config.auto_baud() {
            detect_baud(&port_name).unwrap_or(9600)
        } else {
            config.baud_rate
        };

        let (stop_tx, stop_rx) = mpsc::channel();

        log::info!("Starting GPS reader on {} @ {} baud", port_name, baud);

        std::thread::Builder::new()
            .name("gps-serial".into())
            .spawn(move || {
                gps_read_loop(&port_name, baud, &fix_sender, &stop_rx);
            })
            .expect("failed to spawn gps-serial thread");

        Some(Self {
            _stop_signal: stop_tx,
        })
    }
}

fn gps_read_loop(
    port_name: &str,
    baud: u32,
    fix_sender: &mpsc::Sender<GpsFix>,
    stop_rx: &mpsc::Receiver<()>,
) {
    loop {
        // Check for stop signal
        if stop_rx.try_recv().is_ok() {
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
                log::warn!("Failed to open GPS port {}: {}. Retrying in 5s", port_name, e);
                // Wait 5 seconds before retry, checking for stop signal
                for _ in 0..50 {
                    if stop_rx.try_recv().is_ok() {
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
            if stop_rx.try_recv().is_ok() {
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
                        && fix_sender.send(fix).is_err() {
                            log::info!("GPS fix channel closed, stopping reader");
                            return;
                        }
                }
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                    // Normal timeout, just continue
                    continue;
                }
                Err(e) => {
                    log::warn!("GPS read error: {}. Reconnecting in 5s", e);
                    break;
                }
            }
        }

        // Reconnect delay with stop-signal check
        for _ in 0..50 {
            if stop_rx.try_recv().is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}
