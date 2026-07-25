#![cfg(feature = "serial")]

use std::io::BufRead;
use std::sync::mpsc;
use std::time::Duration;

use crate::config::GpsConfig;
use crate::nmea_parser::NmeaState;
use crate::types::GpsFix;

/// USB VID/PIDs of common GPS chipsets and the USB-serial adapters GPS modules
/// are usually wired through. `None` PID matches every product from that vendor.
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

#[derive(Debug, Clone)]
pub struct GpsPortInfo {
    pub port_name: String,
    pub description: String,
}

/// Ports matching a known GPS VID/PID first, then every other serial port.
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

/// Probe each rate in [`COMMON_BAUDS`] for a line starting with `$`.
fn detect_baud(port_name: &str) -> Option<u32> {
    for &baud in COMMON_BAUDS {
        let port = serialport::new(port_name, baud)
            .timeout(Duration::from_millis(1500))
            .open();
        let Ok(port) = port else { continue };

        let mut reader = std::io::BufReader::new(port);
        let mut buf = String::new();

        for _ in 0..5 {
            buf.clear();
            if reader.read_line(&mut buf).is_ok() && buf.starts_with('$') {
                return Some(baud);
            }
        }
    }
    None
}

/// Owns the reader thread; reconnects on its own after a disconnect.
pub struct SerialGpsReader {
    /// Dropping the sender signals the reader thread to stop.
    _stop_signal: mpsc::Sender<()>,
}

impl SerialGpsReader {
    /// Auto-detects port and baud where `config` leaves them unset. `None` when
    /// no port was found — detection failure, not an error.
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
                // 5s retry, sliced so the stop signal is still seen promptly.
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
                    // A quiet receiver, not a disconnect: do not reconnect.
                    continue;
                }
                Err(e) => {
                    log::warn!("GPS read error: {}. Reconnecting in 5s", e);
                    break;
                }
            }
        }

        // 5s reconnect delay, sliced the same way.
        for _ in 0..50 {
            if stop_rx.try_recv().is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}
