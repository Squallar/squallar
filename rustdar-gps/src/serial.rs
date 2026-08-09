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
    /// # `wake`
    ///
    /// Called after every fix that reaches `fix_sender`, and it is what makes
    /// the fix *visible*. The frontend runs its event loop on
    /// `ControlFlow::Wait` and drains this channel only while rendering a
    /// frame, so a fix pushed from this thread while the app is idle waits for
    /// some unrelated event to produce one — with auto-refresh off, that can be
    /// the next mouse move, or never.
    ///
    /// A bare `impl Fn()` rather than the frontend's `RedrawWaker`: this crate
    /// is a *dependency* of the frontend and cannot name its types. The desktop
    /// bridge passes one through; the shape matches
    /// `ChunkNotifier::sync_sites`, which takes a `wake` for the same reason.
    ///
    /// `Send` and not `Sync`, because the closure is moved into one thread and
    /// shared with none.
    pub fn start(
        config: &GpsConfig,
        fix_sender: mpsc::Sender<GpsFix>,
        wake: impl Fn() + Send + 'static,
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
                gps_read_loop(&port_name, baud, &fix_sender, &stop_rx, &wake);
            })
            .expect("failed to spawn gps-serial thread");

        Some(Self {
            _stop_signal: stop_tx,
        })
    }
}

/// Hand one parsed sentence's outcome to the consumer, and say whether the
/// reader may keep going.
///
/// Split out of [`gps_read_loop`]'s inner match so the pairing can be tested
/// without a serial port: the send and the wake are one step, and a wake that
/// gets separated from its send is a fix that sits in the channel until
/// something else draws a frame — the exact failure this parameter exists for.
/// `false` means the consumer is gone and the thread should stop.
fn deliver(fix: GpsFix, fix_sender: &mpsc::Sender<GpsFix>, wake: &impl Fn()) -> bool {
    if fix_sender.send(fix).is_err() {
        return false;
    }
    wake();
    true
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
    fix_sender: &mpsc::Sender<GpsFix>,
    stop_rx: &mpsc::Receiver<()>,
    wake: &impl Fn(),
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
                        && !deliver(fix, fix_sender, wake)
                    {
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

    /// A counting wake, and the count.
    fn counted() -> (
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        impl Fn() + Send,
    ) {
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let probe = std::sync::Arc::clone(&count);
        (count, move || {
            probe.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })
    }

    fn woke(count: &std::sync::atomic::AtomicUsize) -> usize {
        count.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The bug this parameter exists for. The consumer drains this channel only
    /// while drawing a frame, and its loop runs on `ControlFlow::Wait`: a fix
    /// that lands with nothing else happening sits there until some unrelated
    /// event draws one.
    #[test]
    fn a_fix_arriving_while_the_app_is_idle_asks_for_the_frame_that_shows_it() {
        let (tx, rx) = mpsc::channel();
        let (woken, wake) = counted();

        assert!(deliver(GpsFix::from_lat_lon(35.25, -97.5), &tx, &wake));

        assert_eq!(rx.try_recv().map(|f| f.latitude), Ok(35.25));
        assert_eq!(
            woke(&woken),
            1,
            "the fix reached the channel and nothing asked for the frame that \
             would read it"
        );
    }

    /// The reader stops when the app is gone, and must not wake something that
    /// no longer exists on the way out.
    #[test]
    fn a_fix_with_no_consumer_left_stops_the_reader_without_waking() {
        let (tx, rx) = mpsc::channel();
        drop(rx);
        let (woken, wake) = counted();

        assert!(
            !deliver(GpsFix::from_lat_lon(35.25, -97.5), &tx, &wake),
            "a closed channel must stop the reader"
        );
        assert_eq!(woke(&woken), 0, "woke the loop for a fix nothing received");
    }
}
