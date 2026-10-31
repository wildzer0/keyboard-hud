//! Background thread that owns the keyboard: discovery, reconnection,
//! definition and keymap loading, state polling, and pausing while Vial is
//! running. Results reach the UI as [`Event`]s over a channel.

use super::device::{Device, DeviceError};
use super::mock::MockKeyboard;
use super::protocol::{flags, HudInfo, HudState, ProtocolError};
use super::transport::{self, HidTransport, TransportError};
use crate::backup;
use crate::model::Keymap;
use crate::paths;
use crate::vial::definition::Definition;
use crate::vial::dynamic::Dynamic;
use hidapi::HidApi;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const RETRY_DELAY: Duration = Duration::from_secs(1);
const PAUSE_CHECK: Duration = Duration::from_millis(250);
const VIAL_CHECK_INTERVAL: Duration = Duration::from_secs(2);
const MAX_ERRORS: u32 = 5;
/// After foreign traffic, the keyboard must stay quiet this long before the
/// HUD polls it again.
const FOREIGN_QUIET: Duration = Duration::from_secs(20);
/// Combos, tap dances and macros do not change `keymap_version`: the backup
/// is checked again this often, with the keyboard idle for `BACKUP_IDLE`.
const BACKUP_INTERVAL: Duration = Duration::from_secs(120);
const BACKUP_IDLE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub enum Source {
    Live {
        vid: Option<u16>,
        pid: Option<u16>,
    },
    /// Simulated keyboard, starting `n` ms into its scripted loop.
    Mock(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseReason {
    /// Vial (or VIA) is running and uses the same HID interface.
    Vial,
    /// Answers the HUD did not ask for: another program (Vial in a browser)
    /// is using the keyboard.
    Foreign,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Searching,
    /// Linux: the hidraw node is not readable (missing udev rule).
    PermissionDenied(String),
    Connected {
        product: String,
    },
    /// Keyboard found, but its firmware does not speak the HUD protocol:
    /// keymap read via VIA, no live state.
    NoHudFirmware {
        product: String,
    },
    Paused(PauseReason),
    Error(String),
}

/// One key to write to the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyWrite {
    pub layer: u8,
    pub row: u8,
    pub col: u8,
    pub code: u16,
    /// The keycode the HUD shows there now: the write goes ahead only if the
    /// keyboard stores the same one (not with the Colemak remap in between,
    /// nor after a change made elsewhere).
    pub shown: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteError {
    /// No session with the keyboard (paused, disconnected).
    NotConnected,
    /// The keyboard stores another keycode at that position.
    Remapped,
    /// Backups are on and the one before the write failed.
    Backup,
    Bootloader,
    Device(String),
}

#[derive(Debug, Clone)]
pub enum Event {
    Status(Status),
    Layout {
        definition: Definition,
        layout_options: u32,
        keymap: Keymap,
        /// Tap dances, combos and key overrides (empty if the firmware has none).
        dynamic: Dynamic,
        info: Option<HudInfo>,
    },
    Keymap(Keymap),
    State(HudState),
    /// Outcome of a [`KeyWrite`]: the keycode stored before it, for undo.
    KeyWritten {
        write: KeyWrite,
        result: Result<u16, WriteError>,
    },
    /// A backup was written: the new file, its content and the previous
    /// backup's content (none for the first one).
    Backup {
        path: std::path::PathBuf,
        current: serde_json::Value,
        previous: Option<serde_json::Value>,
        keyboard: String,
    },
}

/// Figures about the link with the keyboard, for the diagnostics window.
#[derive(Debug, Clone, Default)]
pub struct LinkStats {
    /// GET_STATE answers in the last full second.
    pub polls_per_sec: u32,
    /// GET_STATE round trip over the last full second: mean and worst.
    pub latency_avg: Duration,
    pub latency_max: Duration,
    /// Failed requests since the start, and the last one.
    pub errors: u64,
    pub last_error: Option<(String, Instant)>,
    /// Connections made (more than one = it reconnected).
    pub sessions: u32,
    pub connected_since: Option<Instant>,
}

/// GET_STATE answers over one second.
struct Meter {
    start: Instant,
    count: u32,
    total: Duration,
    max: Duration,
}

impl Meter {
    fn new() -> Self {
        Meter { start: Instant::now(), count: 0, total: Duration::ZERO, max: Duration::ZERO }
    }

    /// Adds an answer; after a second, publishes the figures and starts over.
    fn record(&mut self, latency: Duration, ctx: &Ctx) {
        self.count += 1;
        self.total += latency;
        self.max = self.max.max(latency);
        if self.start.elapsed() >= Duration::from_secs(1) {
            ctx.link(|l| {
                l.polls_per_sec = self.count;
                l.latency_avg = self.total / self.count.max(1);
                l.latency_max = self.max;
            });
            *self = Meter::new();
        }
    }
}

/// Settings shared with the HID thread.
struct Control {
    stop: AtomicBool,
    paused: AtomicBool,
    poll_hz: AtomicU32,
    timeout_ms: AtomicU64,
    /// Automatic backups; `None` = off.
    backup: Mutex<Option<backup::Settings>>,
    link: Mutex<LinkStats>,
    writes: Mutex<VecDeque<KeyWrite>>,
}

pub struct Worker {
    pub events: Receiver<Event>,
    control: Arc<Control>,
}

impl Worker {
    /// Starts the worker. `wake` is called after each event (e.g. to request a repaint).
    pub fn start(source: Source, poll_hz: u32, timeout_ms: u64, wake: impl Fn() + Send + 'static) -> Self {
        let (tx, events) = mpsc::channel();
        let control = Arc::new(Control {
            stop: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            poll_hz: AtomicU32::new(poll_hz),
            timeout_ms: AtomicU64::new(timeout_ms),
            backup: Mutex::new(None),
            link: Mutex::new(LinkStats::default()),
            writes: Mutex::new(VecDeque::new()),
        });
        let ctx = Ctx { tx, wake: Box::new(wake), control: control.clone() };
        thread::Builder::new().name("hid".into()).spawn(move || run(source, ctx)).expect("spawn HID thread");
        Worker { events, control }
    }

    pub fn set_poll_hz(&self, hz: u32) {
        self.control.poll_hz.store(hz.clamp(1, 250), Ordering::Relaxed);
    }

    pub fn set_timeout_ms(&self, ms: u64) {
        self.control.timeout_ms.store(ms.clamp(5, 1000), Ordering::Relaxed);
    }

    /// Manual pause: closes the device until resumed.
    pub fn set_paused(&self, paused: bool) {
        self.control.paused.store(paused, Ordering::Relaxed);
    }

    pub fn set_backup(&self, settings: Option<backup::Settings>) {
        if let Ok(mut b) = self.control.backup.lock() {
            *b = settings;
        }
    }

    /// Queues a key to write; the outcome comes back as [`Event::KeyWritten`].
    pub fn write_key(&self, write: KeyWrite) {
        if let Ok(mut w) = self.control.writes.lock() {
            w.push_back(write);
        }
    }

    pub fn is_paused(&self) -> bool {
        self.control.paused.load(Ordering::Relaxed)
    }

    pub fn link(&self) -> LinkStats {
        self.control.link.lock().map(|l| l.clone()).unwrap_or_default()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.control.stop.store(true, Ordering::Relaxed);
    }
}

struct Ctx {
    tx: Sender<Event>,
    wake: Box<dyn Fn() + Send>,
    control: Arc<Control>,
}

impl Ctx {
    /// Returns false when the UI is gone.
    fn send(&self, e: Event) -> bool {
        let ok = self.tx.send(e).is_ok();
        (self.wake)();
        ok
    }

    fn stopped(&self) -> bool {
        self.control.stop.load(Ordering::Relaxed)
    }

    fn link(&self, f: impl FnOnce(&mut LinkStats)) {
        if let Ok(mut l) = self.control.link.lock() {
            f(&mut l);
        }
    }

    fn link_error(&self, e: &DeviceError) {
        self.link(|l| {
            l.errors += 1;
            l.last_error = Some((e.to_string(), Instant::now()));
        });
    }

    fn take_writes(&self) -> Vec<KeyWrite> {
        self.control.writes.lock().map(|mut w| w.drain(..).collect()).unwrap_or_default()
    }

    /// Writes asked while there is no session fail at once.
    fn refuse_writes(&self) -> bool {
        self.take_writes()
            .into_iter()
            .all(|write| self.send(Event::KeyWritten { write, result: Err(WriteError::NotConnected) }))
    }

    fn timeout(&self) -> Duration {
        Duration::from_millis(self.control.timeout_ms.load(Ordering::Relaxed))
    }

    fn sleep(&self, d: Duration) {
        let end = Instant::now() + d;
        while !self.stopped() && Instant::now() < end {
            thread::sleep(Duration::from_millis(10).min(end.saturating_duration_since(Instant::now())));
        }
    }
}

/// Process names that use the keyboard's Raw HID interface.
pub fn is_vial_process(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let n = n.strip_suffix(".exe").unwrap_or(&n);
    // AppImages run as e.g. "Vial-v0.7.5-x86_64.AppImage" (truncated by Linux to 15 chars)
    n == "vial" || n.starts_with("vial-") || n.starts_with("vial.") || n == "via"
}

/// Detects Vial/VIA among the running processes (checked every 2 s).
struct VialWatch {
    system: sysinfo::System,
    last: Option<Instant>,
    running: bool,
}

impl VialWatch {
    fn new() -> Self {
        VialWatch { system: sysinfo::System::new(), last: None, running: false }
    }

    fn running(&mut self) -> bool {
        if self.last.is_some_and(|t| t.elapsed() < VIAL_CHECK_INTERVAL) {
            return self.running;
        }
        self.last = Some(Instant::now());
        self.system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::All,
            true,
            sysinfo::ProcessRefreshKind::nothing(),
        );
        self.running = self.system.processes().values().any(|p| is_vial_process(&p.name().to_string_lossy()));
        self.running
    }
}

struct Connection {
    device: Device,
    product: String,
    info: Option<HudInfo>,
}

enum SessionEnd {
    Lost(Status),
    Paused,
}

fn pause_reason(ctx: &Ctx, vial: &mut Option<VialWatch>) -> Option<PauseReason> {
    if ctx.control.paused.load(Ordering::Relaxed) {
        Some(PauseReason::Manual)
    } else if vial.as_mut().is_some_and(VialWatch::running) {
        Some(PauseReason::Vial)
    } else {
        None
    }
}

/// Sends status changes only (repeated failures are not reported again).
struct Reporter {
    last: Option<Status>,
}

impl Reporter {
    fn report(&mut self, ctx: &Ctx, status: Status) -> bool {
        if self.last.as_ref() == Some(&status) {
            return true;
        }
        log::info!("HID: {status:?}");
        self.last = Some(status.clone());
        ctx.send(Event::Status(status))
    }
}

fn run(source: Source, ctx: Ctx) {
    let mut api = None;
    // the simulated keyboard does not touch the HID interface: no need to yield to Vial
    let mut vial = matches!(source, Source::Live { .. }).then(VialWatch::new);
    let mut reporter = Reporter { last: None };

    while !ctx.stopped() {
        if !ctx.refuse_writes() {
            return;
        }
        if let Some(reason) = pause_reason(&ctx, &mut vial) {
            if !reporter.report(&ctx, Status::Paused(reason)) {
                return;
            }
            ctx.sleep(PAUSE_CHECK);
            continue;
        }
        let status = match connect(&source, &mut api, ctx.timeout()) {
            Ok(conn) => {
                ctx.link(|l| {
                    l.sessions += 1;
                    l.connected_since = Some(Instant::now());
                });
                let end = session(conn, &ctx, &mut vial, &mut reporter);
                ctx.link(|l| {
                    l.connected_since = None;
                    l.polls_per_sec = 0;
                });
                match end {
                    SessionEnd::Paused => continue,
                    SessionEnd::Lost(status) => status,
                }
            }
            Err(status) => status,
        };
        if ctx.stopped() || !reporter.report(&ctx, status) {
            return;
        }
        ctx.sleep(RETRY_DELAY);
    }
}

fn connect(source: &Source, api: &mut Option<HidApi>, timeout: Duration) -> Result<Connection, Status> {
    let (vid, pid) = match source {
        Source::Mock(offset) => {
            let mut device = Device::new(Box::new(MockKeyboard::starting_at(*offset)), timeout);
            let info = device.info().map_err(|e| Status::Error(e.to_string()))?;
            return Ok(Connection { device, product: "Simulated keyboard".into(), info: Some(info) });
        }
        Source::Live { vid, pid } => (*vid, *pid),
    };
    if api.is_none() {
        *api = Some(HidApi::new().map_err(|e| Status::Error(e.to_string()))?);
    }
    let api = api.as_mut().expect("initialized above");
    let candidates = transport::enumerate(api, vid, pid).map_err(|e| Status::Error(e.to_string()))?;
    let mut permission = None;
    let mut fallback = None;
    for c in &candidates {
        let t = match HidTransport::open(api, c) {
            Ok(t) => t,
            Err(TransportError::PermissionDenied(path)) => {
                permission.get_or_insert(path);
                continue;
            }
            Err(e) => {
                log::debug!("{} ({:04x}:{:04x}): {e}", c.product, c.vendor_id, c.product_id);
                continue;
            }
        };
        let mut device = Device::new(Box::new(t), timeout);
        match device.info() {
            Ok(info) => return Ok(Connection { device, product: c.product.clone(), info: Some(info) }),
            Err(DeviceError::Protocol(ProtocolError::Unsupported)) => {
                if fallback.is_none() {
                    fallback = Some(Connection { device, product: c.product.clone(), info: None });
                }
            }
            Err(e) => log::debug!("{}: {e}", c.product),
        }
    }
    if let Some(conn) = fallback {
        return Ok(conn);
    }
    Err(match permission {
        Some(path) => Status::PermissionDenied(path),
        None => Status::Searching,
    })
}

/// Loads the definition, using the on-disk cache when possible.
fn load_definition(device: &mut Device) -> Result<Definition, DeviceError> {
    let (_, uid) = device.vial_id()?;
    let size = device.definition_size()?;
    let cache = paths::definition_cache(uid, size);
    if let Ok(text) = std::fs::read_to_string(&cache) {
        match Definition::from_json_str(&text) {
            Ok(d) => return Ok(d),
            Err(e) => log::warn!("invalid cache {}: {e}", cache.display()),
        }
    }
    let xz = device.definition_xz(size)?;
    let text = Definition::xz_to_json(&xz).map_err(DeviceError::Definition)?;
    let def = Definition::from_json_str(&text).map_err(DeviceError::Definition)?;
    if std::fs::create_dir_all(paths::cache_dir()).and_then(|_| std::fs::write(&cache, &text)).is_err() {
        log::warn!("cannot write the cache {}", cache.display());
    }
    Ok(def)
}

fn read_keymap(conn: &mut Connection, def: &Definition) -> Result<Keymap, DeviceError> {
    match &conn.info {
        Some(info) => conn.device.keymap(info),
        None => {
            let layers = conn.device.layer_count()?;
            conn.device.via_keymap(layers, def.rows, def.cols)
        }
    }
}

/// Runs one connected session until the keyboard goes away or a pause is
/// requested (the device is then closed by dropping `conn`).
fn session(mut conn: Connection, ctx: &Ctx, vial: &mut Option<VialWatch>, reporter: &mut Reporter) -> SessionEnd {
    let setup = (|| -> Result<(Definition, u32, Keymap), DeviceError> {
        let def = load_definition(&mut conn.device)?;
        let options = conn.device.layout_options().unwrap_or(0);
        let keymap = read_keymap(&mut conn, &def)?;
        Ok((def, options, keymap))
    })();
    let (definition, layout_options, keymap) = match setup {
        Ok(v) => v,
        Err(e) => return SessionEnd::Lost(Status::Error(e.to_string())),
    };
    // read at every connection: Vial changes them while the HUD is paused
    let mut dynamic = match conn.device.dynamic() {
        Ok(d) => d,
        Err(DeviceError::Protocol(ProtocolError::Unsupported)) => Dynamic::default(),
        Err(e) => {
            log::warn!("tap dances, combos and key overrides not read: {e}");
            Dynamic::default()
        }
    };
    dynamic.macros = conn.device.macros().unwrap_or_else(|e| {
        log::warn!("macros not read: {e}");
        Vec::new()
    });
    log::info!(
        "Vial: {} tap dances, {} combos, {} key overrides, {} macros",
        dynamic.tap_dances.iter().filter(|t| !t.is_empty()).count(),
        dynamic.combos.len(),
        dynamic.key_overrides.len(),
        dynamic.macros.iter().filter(|m| !m.is_empty()).count()
    );
    let status = match conn.info {
        Some(_) => Status::Connected { product: conn.product.clone() },
        None => Status::NoHudFirmware { product: conn.product.clone() },
    };
    // force the status event: the UI drops the layout on any non-connected status
    reporter.last = None;
    if !reporter.report(ctx, status)
        || !ctx.send(Event::Layout { definition: definition.clone(), layout_options, keymap, dynamic, info: conn.info })
    {
        return SessionEnd::Lost(Status::Searching);
    }

    // first connection, Vial closed, foreign traffic over: the configuration may have changed
    run_backup(&mut conn, &definition, ctx);
    let mut backup_checked = Instant::now();
    let mut backup_due = false;
    let mut last_activity = Instant::now();

    let mut errors = 0;
    let mut meter = Meter::new();
    let mut last: Option<HudState> = None;
    // answers to our own setup requests may still be counted
    conn.device.take_foreign();
    while !ctx.stopped() {
        let started = Instant::now();
        if pause_reason(ctx, vial).is_some() {
            log::info!("HID: device closed (paused)");
            return SessionEnd::Paused;
        }
        for write in ctx.take_writes() {
            let result = write_key(&mut conn, &definition, ctx, write);
            if result.is_ok() {
                match read_keymap(&mut conn, &definition) {
                    Ok(km) => {
                        if !ctx.send(Event::Keymap(km)) {
                            return SessionEnd::Lost(Status::Searching);
                        }
                    }
                    Err(e) => log::warn!("keymap re-read failed: {e}"),
                }
            }
            if !ctx.send(Event::KeyWritten { write, result }) {
                return SessionEnd::Lost(Status::Searching);
            }
            conn.device.take_foreign();
        }
        let idle = last.is_none_or(|s| s.matrix.is_empty()) && last_activity.elapsed() >= BACKUP_IDLE;
        if (backup_due || backup_checked.elapsed() >= BACKUP_INTERVAL) && (idle || conn.info.is_none()) {
            run_backup(&mut conn, &definition, ctx);
            conn.device.take_foreign();
            backup_checked = Instant::now();
            backup_due = false;
        }
        if conn.info.is_none() {
            // Firmware without HUD: just check now and then that the keyboard is still there.
            if conn.device.layer_count().is_err() {
                return SessionEnd::Lost(Status::Searching);
            }
            ctx.sleep(Duration::from_secs(1));
            continue;
        }
        let asked = Instant::now();
        let result = match &conn.info {
            Some(info) => conn.device.state(info),
            None => continue,
        };
        let latency = asked.elapsed();
        let foreign = conn.device.take_foreign();
        if foreign > 0 {
            log::info!("answers meant for another program ({foreign}): paused until the keyboard is free");
            return foreign_pause(&mut conn, ctx, vial, reporter);
        }
        match result {
            Ok(state) => {
                errors = 0;
                meter.record(latency, ctx);
                let keymap_changed = last.is_some_and(|l| {
                    l.keymap_version != state.keymap_version || l.flag(flags::COLEMAK) != state.flag(flags::COLEMAK)
                });
                if !state.matrix.is_empty() {
                    last_activity = Instant::now();
                }
                backup_due |= keymap_changed;
                // only changes reach the UI: an idle keyboard costs no repaints
                let changed = last != Some(state);
                last = Some(state);
                if changed && !ctx.send(Event::State(state)) {
                    return SessionEnd::Lost(Status::Searching);
                }
                if keymap_changed {
                    log::info!("keymap changed (version {}): re-reading", state.keymap_version);
                    match read_keymap(&mut conn, &definition) {
                        Ok(km) => {
                            if !ctx.send(Event::Keymap(km)) {
                                return SessionEnd::Lost(Status::Searching);
                            }
                        }
                        Err(e) => log::warn!("keymap re-read failed: {e}"),
                    }
                }
            }
            Err(DeviceError::Protocol(ProtocolError::Busy)) => {}
            Err(e) => {
                errors += 1;
                ctx.link_error(&e);
                log::debug!("GET_STATE: {e}");
                if errors >= MAX_ERRORS {
                    log::warn!("the keyboard does not answer: reconnecting");
                    return SessionEnd::Lost(Status::Searching);
                }
            }
        }
        let hz = ctx.control.poll_hz.load(Ordering::Relaxed).max(1);
        ctx.sleep(Duration::from_secs_f64(1.0 / hz as f64).saturating_sub(started.elapsed()));
    }
    SessionEnd::Lost(Status::Searching)
}

/// Writes one key, after a backup of the configuration as it is now.
/// Returns the keycode stored before.
fn write_key(conn: &mut Connection, def: &Definition, ctx: &Ctx, w: KeyWrite) -> Result<u16, WriteError> {
    let device = |e: DeviceError| WriteError::Device(e.to_string());
    if w.code == crate::keymap_guard::QK_BOOT {
        return Err(WriteError::Bootloader);
    }
    let stored = conn.device.keycode(w.layer, w.row, w.col).map_err(device)?;
    if stored != w.shown {
        return Err(WriteError::Remapped);
    }
    if !run_backup(conn, def, ctx) {
        return Err(WriteError::Backup);
    }
    conn.device.set_keycode(w.layer, w.row, w.col, w.code).map_err(device)?;
    log::info!("key written: layer {}, row {}, column {}: {stored:#06x} → {:#06x}", w.layer, w.row, w.col, w.code);
    Ok(stored)
}

/// Reads the whole configuration and writes a `.vil` if it differs from the
/// last backup. Failures are logged only: the HUD keeps working. Returns
/// false if the backup failed (true when backups are off).
fn run_backup(conn: &mut Connection, def: &Definition, ctx: &Ctx) -> bool {
    let Some(settings) = ctx.control.backup.lock().ok().and_then(|b| b.clone()) else { return true };
    let started = Instant::now();
    let dump = match conn.device.dump(def) {
        Ok(d) => d,
        Err(e) => {
            log::warn!("backup: reading the configuration failed: {e}");
            return false;
        }
    };
    let vil = dump.to_vil();
    match backup::save_if_changed(&settings, &def.name, &vil, crate::clock::now_secs()) {
        Ok(Some((path, previous))) => {
            log::info!("backup saved: {} ({} ms)", path.display(), started.elapsed().as_millis());
            ctx.send(Event::Backup { path, current: vil, previous, keyboard: def.name.clone() });
            true
        }
        Ok(None) => {
            log::debug!("backup: no changes ({} ms)", started.elapsed().as_millis());
            true
        }
        Err(e) => {
            log::warn!("backup in {}: {e}", settings.dir.display());
            false
        }
    }
}

/// Stops polling while another program talks to the keyboard, but keeps
/// listening. After `FOREIGN_QUIET` of silence the session ends, so that the
/// next one reloads everything that program may have changed.
fn foreign_pause(
    conn: &mut Connection,
    ctx: &Ctx,
    vial: &mut Option<VialWatch>,
    reporter: &mut Reporter,
) -> SessionEnd {
    if !reporter.report(ctx, Status::Paused(PauseReason::Foreign)) {
        return SessionEnd::Lost(Status::Searching);
    }
    let mut last = Instant::now();
    while !ctx.stopped() {
        if pause_reason(ctx, vial).is_some() {
            return SessionEnd::Paused;
        }
        match conn.device.listen(PAUSE_CHECK) {
            Ok(0) => {}
            Ok(_) => last = Instant::now(),
            Err(_) => return SessionEnd::Lost(Status::Searching),
        }
        if last.elapsed() >= FOREIGN_QUIET {
            log::info!("keyboard free again: resuming");
            return SessionEnd::Paused;
        }
    }
    SessionEnd::Lost(Status::Searching)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vial_process_names() {
        for yes in ["Vial", "Vial.exe", "vial", "Vial-v0.7.5-x8", "VIA.exe", "via"] {
            assert!(is_vial_process(yes), "{yes}");
        }
        for no in ["keyboard-hud", "viable", "trivial", "vialtools", "Via Ferrata"] {
            assert!(!is_vial_process(no), "{no}");
        }
    }

    #[test]
    fn process_list_is_readable() {
        let mut w = VialWatch::new();
        let _ = w.running();
        assert!(!w.system.processes().is_empty(), "sysinfo returned no processes");
    }

    #[test]
    fn session_writes_a_backup_once() {
        let dir = std::env::temp_dir().join(format!("keyboard-hud-worker-backup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let w = Worker::start(Source::Mock(0), 100, 50, || {});
        w.set_backup(Some(backup::Settings { dir: dir.clone(), keep: 5 }));
        // the setting may arrive after the first session has started: pause and resume
        w.set_paused(true);
        thread::sleep(Duration::from_millis(400));
        w.set_paused(false);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut path = None;
        while Instant::now() < deadline && path.is_none() {
            if let Ok(Event::Backup { path: p, .. }) = w.events.recv_timeout(Duration::from_millis(100)) {
                path = Some(p);
            }
        }
        let path = path.expect("no backup event");
        let text = std::fs::read_to_string(&path).unwrap();
        let vil = crate::vial::vil::Vil::parse(&text, 8, 7).unwrap();
        assert_eq!(vil.dynamic.combos.len(), 3);
        // a new session with nothing changed writes nothing
        w.set_paused(true);
        thread::sleep(Duration::from_millis(400));
        w.set_paused(false);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Ok(Event::Backup { path: p, .. }) = w.events.recv_timeout(Duration::from_millis(100)) {
                panic!("unexpected backup {}", p.display());
            }
        }
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        drop(w);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keys_are_written_only_when_shown_and_connected() {
        let w = Worker::start(Source::Mock(0), 100, 50, || {});
        let next = |pred: &dyn Fn(&Event) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if let Ok(e) = w.events.recv_timeout(Duration::from_millis(100)) {
                    if pred(&e) {
                        return Some(e);
                    }
                }
            }
            None
        };
        let Some(Event::Layout { keymap, .. }) = next(&|e| matches!(e, Event::Layout { .. })) else {
            panic!("no layout")
        };
        let shown = keymap.get(0, 1, 1);
        let write = KeyWrite { layer: 0, row: 1, col: 1, code: 0x29, shown };
        w.write_key(write);
        let Some(Event::Keymap(km)) = next(&|e| matches!(e, Event::Keymap(_))) else { panic!("no keymap") };
        assert_eq!(km.get(0, 1, 1), 0x29);
        let Some(Event::KeyWritten { result, .. }) = next(&|e| matches!(e, Event::KeyWritten { .. })) else {
            panic!("no answer")
        };
        assert_eq!(result, Ok(shown));
        // the HUD shows something else than the keyboard stores: refused
        w.write_key(KeyWrite { code: 0x2A, ..write });
        let Some(Event::KeyWritten { result, .. }) = next(&|e| matches!(e, Event::KeyWritten { .. })) else {
            panic!("no answer")
        };
        assert_eq!(result, Err(WriteError::Remapped));
        // paused: refused at once
        w.set_paused(true);
        next(&|e| matches!(e, Event::Status(Status::Paused(_)))).expect("not paused");
        w.write_key(KeyWrite { code: 0x2A, shown: 0x29, ..write });
        let Some(Event::KeyWritten { result, .. }) = next(&|e| matches!(e, Event::KeyWritten { .. })) else {
            panic!("no answer")
        };
        assert_eq!(result, Err(WriteError::NotConnected));
    }

    #[test]
    fn mock_session_delivers_layout_states_and_pauses() {
        let w = Worker::start(Source::Mock(0), 100, 50, || {});
        let mut got_layout = false;
        let mut states = 0;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !(got_layout && states >= 3) {
            match w.events.recv_timeout(Duration::from_millis(100)) {
                Ok(Event::Layout { keymap, .. }) => {
                    assert_eq!(keymap.layers(), 6);
                    got_layout = true;
                }
                Ok(Event::State(_)) => states += 1,
                _ => {}
            }
        }
        assert!(got_layout && states >= 3, "layout {got_layout}, states {states}");

        // manual pause closes the session and reports it; resuming reloads the layout
        w.set_paused(true);
        assert!(w.is_paused());
        let wait_for = |pred: &dyn Fn(&Event) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                if let Ok(e) = w.events.recv_timeout(Duration::from_millis(100)) {
                    if pred(&e) {
                        return true;
                    }
                }
            }
            false
        };
        assert!(wait_for(&|e| matches!(e, Event::Status(Status::Paused(PauseReason::Manual)))));
        w.set_paused(false);
        assert!(wait_for(&|e| matches!(e, Event::Layout { .. })));
    }
}
