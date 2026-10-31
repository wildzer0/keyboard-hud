//! High-level keyboard access over any [`Transport`]: HUD requests with
//! sequence numbers and timeouts, plus the VIA/Vial reads the app needs.

use super::protocol::{self as p, HudCommand, HudInfo, HudState, ProtocolError, Report};
use super::transport::{Transport, TransportError};
use crate::i18n::pick;
use crate::kle::KeyRole;
use crate::model::Keymap;
use crate::tr_format;
use crate::vial::definition::Definition;
use crate::vial::definition::DefinitionError;
use crate::vial::dump::{decode_macros, qmk_setting_width, AltRepeat, Dump};
use crate::vial::dynamic::{Combo, Dynamic, KeyOverride, TapDance, ENTRY_SIZE};
use std::collections::BTreeMap;
use std::fmt;
use std::time::{Duration, Instant};

/// Upper bound for the compressed definition (the Corne's is ~2 KiB).
const MAX_DEFINITION_SIZE: u32 = 256 * 1024;

#[derive(Debug)]
pub enum DeviceError {
    Transport(TransportError),
    Protocol(ProtocolError),
    Timeout,
    Definition(DefinitionError),
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeviceError::Transport(e) => e.fmt(f),
            DeviceError::Protocol(e) => e.fmt(f),
            DeviceError::Timeout => write!(f, "{}", pick("la tastiera non risponde", "the keyboard does not answer")),
            DeviceError::Definition(e) => write!(f, "{}", tr_format!("definizione Vial: {e}", "Vial definition: {e}")),
        }
    }
}

impl std::error::Error for DeviceError {}

impl From<TransportError> for DeviceError {
    fn from(e: TransportError) -> Self {
        DeviceError::Transport(e)
    }
}

impl From<ProtocolError> for DeviceError {
    fn from(e: ProtocolError) -> Self {
        DeviceError::Protocol(e)
    }
}

/// Dynamic entries as stored, empty ones included.
#[derive(Debug, Clone, Default)]
pub struct DynamicRaw {
    pub tap_dances: Vec<TapDance>,
    pub combos: Vec<Combo>,
    pub key_overrides: Vec<KeyOverride>,
    pub alt_repeat: Vec<AltRepeat>,
}

pub struct Device {
    transport: Box<dyn Transport>,
    seq: u8,
    timeout: Duration,
    /// Reports that are not HUD answers: another program (e.g. Vial in a
    /// browser, via WebHID) is talking to the keyboard. Every program that
    /// opens the interface receives every answer.
    foreign: u32,
}

impl Device {
    pub fn new(transport: Box<dyn Transport>, timeout: Duration) -> Self {
        Device { transport, seq: 0, timeout, foreign: 0 }
    }

    fn note(&mut self, r: &Report) {
        if r[0] != p::HUD_CMD {
            self.foreign += 1;
        }
    }

    /// Discards queued reports (e.g. answers to timed-out requests), counting
    /// the foreign ones.
    fn drain(&mut self) -> Result<(), DeviceError> {
        for _ in 0..64 {
            match self.transport.recv(Duration::ZERO)? {
                Some(r) => self.note(&r),
                None => break,
            }
        }
        Ok(())
    }

    /// Foreign reports seen since the last call. Meaningful while only HUD
    /// requests are sent (the polling loop).
    pub fn take_foreign(&mut self) -> u32 {
        std::mem::take(&mut self.foreign)
    }

    /// Waits up to `timeout` without sending anything and returns how many
    /// reports arrived: all of them belong to another program.
    pub fn listen(&mut self, timeout: Duration) -> Result<u32, DeviceError> {
        let deadline = Instant::now() + timeout;
        let mut n = 0;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.transport.recv(left)? {
                Some(_) => n += 1,
                None => return Ok(n),
            }
            if left.is_zero() {
                return Ok(n);
            }
        }
    }

    fn next_seq(&mut self) -> u8 {
        self.seq = self.seq.wrapping_add(1);
        self.seq
    }

    /// Sends a HUD request and waits for the matching response, skipping
    /// stale reports.
    fn hud(&mut self, cmd: HudCommand, args: &[u8]) -> Result<(Report, u8), DeviceError> {
        let seq = self.next_seq();
        self.drain()?;
        self.transport.send(&p::hud_request(cmd, seq, args))?;
        let deadline = Instant::now() + self.timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.transport.recv(left)? {
                Some(r) if p::hud_matches(&r, cmd, seq) => return Ok((r, seq)),
                Some(r) => self.note(&r),
                None if Instant::now() >= deadline => return Err(DeviceError::Timeout),
                None => continue,
            }
        }
    }

    /// Sends a VIA/Vial request and returns the first response. VIA echoes the
    /// command id in byte 0; Vial overwrites the whole buffer, so `expect` is
    /// `None` for Vial commands.
    fn via(&mut self, req: &Report, expect: Option<u8>) -> Result<Report, DeviceError> {
        self.drain()?;
        self.transport.send(req)?;
        let deadline = Instant::now() + self.timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.transport.recv(left)? {
                Some(r) if expect.is_none_or(|id| r[0] == id) => return Ok(r),
                Some(r) => self.note(&r),
                None if Instant::now() >= deadline => return Err(DeviceError::Timeout),
                None => continue,
            }
        }
    }

    pub fn info(&mut self) -> Result<HudInfo, DeviceError> {
        let (r, seq) = self.hud(HudCommand::Info, &[])?;
        Ok(p::decode_info(&r, seq)?)
    }

    /// Layers, modifiers and flags; with protocol 2 the pressed keys of the
    /// whole matrix come from a second request.
    pub fn state(&mut self, info: &HudInfo) -> Result<HudState, DeviceError> {
        let (r, seq) = self.hud(HudCommand::State, &[])?;
        let mut state = p::decode_state(&r, seq)?;
        if info.has_matrix_command() {
            let (r, seq) = self.hud(HudCommand::Matrix, &[])?;
            state.matrix = p::decode_matrix(&r, seq, info.rows, info.row_bytes)?;
        }
        Ok(state)
    }

    /// Effective keymap (Colemak remap already applied by the firmware).
    pub fn keymap(&mut self, info: &HudInfo) -> Result<Keymap, DeviceError> {
        let mut km = Keymap::new(info.layers, info.rows, info.cols);
        let total = info.rows as u16 * info.cols as u16;
        for layer in 0..info.layers {
            let mut offset = 0u16;
            while offset < total {
                let count = (info.keymap_chunk as u16).min(total - offset) as u8;
                let o = offset.to_le_bytes();
                let (r, seq) = self.hud(HudCommand::Keymap, &[layer, o[0], o[1], count])?;
                let codes = p::decode_keymap(&r, seq, layer, offset, count)?;
                km.set_run(layer, offset, &codes);
                offset += count as u16;
            }
        }
        Ok(km)
    }

    /// Raw keymap via VIA `dynamic_keymap_get_buffer` (no Colemak remap).
    /// Used when the firmware does not speak the HUD protocol.
    pub fn via_keymap(&mut self, layers: u8, rows: u8, cols: u8) -> Result<Keymap, DeviceError> {
        let mut km = Keymap::new(layers, rows, cols);
        let size = layers as usize * rows as usize * cols as usize * 2;
        let mut bytes = Vec::with_capacity(size);
        while bytes.len() < size {
            let offset = bytes.len() as u16;
            let n = (size - bytes.len()).min(28) as u8;
            let o = offset.to_be_bytes();
            let r = self.via(&p::via_request(&[0x12, o[0], o[1], n]), Some(0x12))?;
            if r[1] != o[0] || r[2] != o[1] || r[3] != n {
                return Err(ProtocolError::Mismatch.into());
            }
            bytes.extend_from_slice(&r[4..4 + n as usize]);
        }
        let per_layer = rows as usize * cols as usize;
        for (i, pair) in bytes.chunks_exact(2).enumerate() {
            let (layer, pos) = (i / per_layer, i % per_layer);
            km.set(
                layer as u8,
                (pos / cols as usize) as u8,
                (pos % cols as usize) as u8,
                u16::from_be_bytes([pair[0], pair[1]]),
            );
        }
        Ok(km)
    }

    /// Tap dances, combos and key overrides configured in Vial (unused ones
    /// dropped). Reading them does not need the Vial unlock.
    pub fn dynamic(&mut self) -> Result<Dynamic, DeviceError> {
        let r = self.dynamic_raw()?;
        Ok(Dynamic::new(r.tap_dances, r.combos, r.key_overrides))
    }

    /// Every dynamic entry, empty ones included.
    pub fn dynamic_raw(&mut self) -> Result<DynamicRaw, DeviceError> {
        let req = p::dynamic_counts_request();
        let [tap_dances, combos, overrides, alt_repeat] = p::decode_dynamic_counts(&req, &self.via(&req, None)?)?;
        let mut read = |kind: p::DynamicKind, n: u8| -> Result<Vec<[u8; ENTRY_SIZE]>, DeviceError> {
            (0..n)
                .map(|i| {
                    let req = p::dynamic_entry_request(kind, i);
                    Ok(p::decode_dynamic_entry(&req, &self.via(&req, None)?)?)
                })
                .collect()
        };
        Ok(DynamicRaw {
            tap_dances: read(p::DynamicKind::TapDance, tap_dances)?.iter().map(TapDance::from_bytes).collect(),
            combos: read(p::DynamicKind::Combo, combos)?.iter().map(Combo::from_bytes).collect(),
            key_overrides: read(p::DynamicKind::KeyOverride, overrides)?.iter().map(KeyOverride::from_bytes).collect(),
            alt_repeat: read(p::DynamicKind::AltRepeat, alt_repeat)?.iter().map(|b| AltRepeat::from_bytes(b)).collect(),
        })
    }

    /// VIA protocol version (big-endian in bytes 1-2).
    pub fn via_protocol(&mut self) -> Result<u16, DeviceError> {
        let r = self.via(&p::via_request(&[p::VIA_GET_PROTOCOL_VERSION]), Some(p::VIA_GET_PROTOCOL_VERSION))?;
        Ok(u16::from_be_bytes([r[1], r[2]]))
    }

    /// Macros: count and buffer through VIA, decoded. A firmware without
    /// dynamic macros has none.
    pub fn macros(&mut self) -> Result<Vec<Vec<crate::vial::dump::MacroAction>>, DeviceError> {
        let r = self.via(&p::via_request(&[p::VIA_MACRO_COUNT]), None)?;
        if r[0] != p::VIA_MACRO_COUNT {
            return Ok(Vec::new());
        }
        let count = r[1] as usize;
        let r = self.via(&p::via_request(&[p::VIA_MACRO_BUFFER_SIZE]), Some(p::VIA_MACRO_BUFFER_SIZE))?;
        let size = u16::from_be_bytes([r[1], r[2]]) as usize;
        let mut buffer = Vec::with_capacity(size);
        while buffer.len() < size {
            let o = (buffer.len() as u16).to_be_bytes();
            let n = (size - buffer.len()).min(28) as u8;
            let r = self.via(&p::via_request(&[p::VIA_MACRO_BUFFER, o[0], o[1], n]), Some(p::VIA_MACRO_BUFFER))?;
            if r[1] != o[0] || r[2] != o[1] || r[3] != n {
                return Err(ProtocolError::Mismatch.into());
            }
            buffer.extend_from_slice(&r[4..4 + n as usize]);
            // every macro already ended: the rest is free space
            if buffer.iter().filter(|&&b| b == 0).count() >= count {
                break;
            }
        }
        Ok(decode_macros(&buffer, count))
    }

    /// Encoder keycodes `[ccw, cw]` for `count` encoders on `layers` layers.
    pub fn encoders(&mut self, layers: u8, count: u8) -> Result<Vec<Vec<[u16; 2]>>, DeviceError> {
        (0..layers)
            .map(|layer| {
                (0..count)
                    .map(|idx| {
                        let req = p::via_request(&[p::VIAL_PREFIX, p::VIAL_GET_ENCODER, layer, idx]);
                        let r = self.via(&req, None)?;
                        if r == req {
                            return Err(ProtocolError::Unsupported.into());
                        }
                        Ok([u16::from_be_bytes([r[0], r[1]]), u16::from_be_bytes([r[2], r[3]])])
                    })
                    .collect()
            })
            .collect()
    }

    /// QMK settings (tapping term, ...) by id; empty without `QMK_SETTINGS`.
    pub fn qmk_settings(&mut self) -> Result<BTreeMap<u16, u32>, DeviceError> {
        let mut ids = Vec::new();
        let mut after = 0u16;
        loop {
            let g = after.to_le_bytes();
            let req = p::via_request(&[p::VIAL_PREFIX, p::VIAL_QMK_SETTINGS_QUERY, g[0], g[1]]);
            let r = self.via(&req, None)?;
            if r == req {
                break;
            }
            let batch: Vec<u16> = r
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .take_while(|&id| id != 0xFFFF && id > after)
                .collect();
            match batch.last() {
                Some(&last) => {
                    ids.extend_from_slice(&batch);
                    after = last;
                }
                None => break,
            }
        }
        let mut settings = BTreeMap::new();
        for id in ids {
            let Some(width) = qmk_setting_width(id) else {
                log::debug!("QMK setting {id}: unknown size, not saved");
                continue;
            };
            let q = id.to_le_bytes();
            let r = self.via(&p::via_request(&[p::VIAL_PREFIX, p::VIAL_QMK_SETTINGS_GET, q[0], q[1]]), None)?;
            if r[0] != 0 {
                continue;
            }
            let mut v = [0u8; 4];
            v[..width].copy_from_slice(&r[1..1 + width]);
            settings.insert(id, u32::from_le_bytes(v));
        }
        Ok(settings)
    }

    /// Everything a `.vil` holds, read from the keyboard.
    pub fn dump(&mut self, def: &Definition) -> Result<Dump, DeviceError> {
        let (vial_protocol, uid) = self.vial_id()?;
        let layers = self.layer_count()?;
        let keymap = self.via_keymap(layers, def.rows, def.cols)?;
        let mut present = Vec::new();
        let mut encoders = 0u8;
        for k in &def.keys {
            match k.role() {
                Some(KeyRole::Matrix { row, col }) if !present.contains(&(row, col)) => present.push((row, col)),
                Some(KeyRole::Encoder { index, .. }) => encoders = encoders.max(index + 1),
                _ => {}
            }
        }
        let encoders = match self.encoders(layers, encoders) {
            Ok(e) => e,
            Err(DeviceError::Protocol(ProtocolError::Unsupported)) => Vec::new(),
            Err(e) => return Err(e),
        };
        let dynamic = match self.dynamic_raw() {
            Ok(d) => d,
            Err(DeviceError::Protocol(ProtocolError::Unsupported)) => DynamicRaw::default(),
            Err(e) => return Err(e),
        };
        Ok(Dump {
            uid,
            vial_protocol,
            via_protocol: self.via_protocol()?,
            layout_options: self.layout_options()?,
            keymap,
            present,
            encoders,
            macros: self.macros()?,
            tap_dances: dynamic.tap_dances,
            combos: dynamic.combos,
            key_overrides: dynamic.key_overrides,
            alt_repeat: dynamic.alt_repeat,
            settings: self.qmk_settings()?,
        })
    }

    /// Stored keycode of one key (VIA, no Colemak remap).
    pub fn keycode(&mut self, layer: u8, row: u8, col: u8) -> Result<u16, DeviceError> {
        let req = p::get_keycode_request(layer, row, col);
        Ok(p::decode_keycode(&req, &self.via(&req, Some(p::VIA_GET_KEYCODE))?)?)
    }

    /// Writes one key and reads it back.
    pub fn set_keycode(&mut self, layer: u8, row: u8, col: u8, code: u16) -> Result<(), DeviceError> {
        let req = p::set_keycode_request(layer, row, col, code);
        p::check_set_keycode(&req, &self.via(&req, Some(p::VIA_SET_KEYCODE))?)?;
        if self.keycode(layer, row, col)? != code {
            return Err(ProtocolError::Invalid("key not written").into());
        }
        Ok(())
    }

    pub fn layer_count(&mut self) -> Result<u8, DeviceError> {
        let r = self.via(&p::layer_count_request(), Some(p::VIA_GET_LAYER_COUNT))?;
        let n = p::decode_layer_count(&r)?;
        if n == 0 || n > 32 {
            return Err(ProtocolError::Invalid("layer count").into());
        }
        Ok(n)
    }

    pub fn layout_options(&mut self) -> Result<u32, DeviceError> {
        let r = self.via(&p::layout_options_request(), Some(p::VIA_GET_KEYBOARD_VALUE))?;
        Ok(p::decode_layout_options(&r)?)
    }

    /// `(vial_protocol, keyboard uid)`.
    pub fn vial_id(&mut self) -> Result<(u32, u64), DeviceError> {
        let r = self.via(&p::vial_id_request(), None)?;
        Ok(p::decode_vial_id(&r))
    }

    pub fn definition_size(&mut self) -> Result<u32, DeviceError> {
        let r = self.via(&p::vial_size_request(), None)?;
        let size = p::decode_vial_size(&r);
        if size == 0 || size > MAX_DEFINITION_SIZE {
            return Err(ProtocolError::Invalid("definition size").into());
        }
        Ok(size)
    }

    /// Downloads the compressed definition (`size` from [`Self::definition_size`]).
    pub fn definition_xz(&mut self, size: u32) -> Result<Vec<u8>, DeviceError> {
        let pages = size.div_ceil(p::REPORT_SIZE as u32);
        let mut data = Vec::with_capacity(pages as usize * p::REPORT_SIZE);
        for page in 0..pages {
            let page = u16::try_from(page).map_err(|_| ProtocolError::Invalid("page"))?;
            let r = self.via(&p::vial_def_request(page), None)?;
            data.extend_from_slice(&r);
        }
        data.truncate(size as usize);
        Ok(data)
    }
}
