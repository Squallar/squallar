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

/// Probe each rate in [`COMMON_BAUDS`] for a line starting with `$`. Worst
/// case is tens of seconds of blocking reads, so it runs on the reader thread
/// and checks the stop signal between reads; `None` on stop is fine — the
/// caller falls back to a default and [`gps_read_loop`] re-checks immediately.
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

/// Owns the reader thread; reconnects on its own after a disconnect.
pub struct SerialGpsReader {
    /// Dropping the sender signals the reader thread to stop.
    _stop_signal: mpsc::Sender<()>,
}

impl SerialGpsReader {
    /// Spawns the reader thread and returns it immediately — always `Some`
    /// today. Port and baud auto-detection (where `config` leaves them unset)
    /// happens on that thread, not here — probing can block for tens of
    /// seconds and the caller is the GUI thread — so the reader is returned
    /// before any port is confirmed; if none ever turns up, the thread keeps
    /// retrying detection until the reader is dropped.
    ///
    /// # `on_fix`
    ///
    /// Called on the reader thread with every parsed position-bearing
    /// sentence. Returning `false` means the consumer is gone and stops the
    /// thread — which holds the port open exclusively, so a consumer that
    /// vanishes without stopping the reader would pin the device forever.
    ///
    /// A callback rather than a channel of this crate's own, because what a
    /// consumer does with a parsed fix — translate it into an app fix model,
    /// send it somewhere, wake an event loop so the send is *seen* — is the
    /// consumer's business, and since WO-RL-3 that consumer is
    /// `rustdar_location`'s `serial` module, which this crate deliberately
    /// knows nothing about.
    ///
    /// `Send` and not `Sync`, because the closure is moved into one thread and
    /// shared with none. `FnMut`, because a translating consumer may keep
    /// state.
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
/// every 5s so a receiver plugged in after enabling GPS is still picked up.
/// `None` only when stopped while waiting.
fn resolve_port(configured: Option<String>, stop_rx: &mpsc::Receiver<()>) -> Option<String> {
    if let Some(path) = configured {
        return Some(path);
    }
    loop {
        if let Some(port) = detect_gps_ports().first() {
            return Some(port.port_name.clone());
        }
        log::warn!("No GPS port found. Retrying detection in 5s");
        // 5s retry, sliced so the stop signal is still seen promptly.
        for _ in 0..50 {
            if should_stop(stop_rx) {
                return None;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Whether the reader thread must exit: an explicit stop `()` arrived, or the
/// [`SerialGpsReader`] holding the sender was dropped (`Disconnected`). Only
/// an empty-but-connected channel means keep running — a bare
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
                // 5s retry, sliced so the stop signal is still seen promptly.
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

        // 5s reconnect delay, sliced the same way.
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
    /// the thread must read `Disconnected` as stop — a bare `is_ok()` check
    /// here leaked the thread and kept the port open exclusively forever.
    #[test]
    fn should_stop_fires_when_the_sender_is_dropped_without_sending() {
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        drop(stop_tx);
        assert!(should_stop(&stop_rx));
    }

    // The send-plus-wake delivery pairing and its two tests moved to
    // `rustdar_location::serial` at WO-RL-3 with the `deliver` fn they test:
    // sending an app fix and waking an event loop are the *consumer's* step,
    // and the consumer of this transport is the facade's serial module.
}
