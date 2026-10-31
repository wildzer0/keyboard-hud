//! Simulated keyboard (`--mock`): answers HUD, VIA and Vial requests like
//! the real firmware, with a scripted loop of realistic states (typing,
//! Fn1/Fn2/tri-layer, one-shot ACC, mod-tap Ctrl, Caps Word, DEBUG lock).

use super::protocol::{self as p, flags, Report, REPORT_SIZE};
use super::transport::{Transport, TransportError};
use crate::model::Keymap;
use crate::vial::dynamic::{Combo, Dynamic};
use crate::vial::vil::Vil;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub const MOCK_VIAL_JSON: &str = include_str!("../../data/mock/vial.json");
pub const MOCK_VIL: &str = include_str!("../../data/mock/corne.vil");
// "MOCK" + 1: distinct from real keyboards, so the definition cache never mixes them
const MOCK_UID: u64 = 0x4D4F_434B_0000_0001;

/// Simulated firmware state at one instant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MockFrame {
    pub layer_state: u32,
    pub pressed: [u8; 8],
    pub mods: u8,
    pub oneshot_layer: Option<u8>,
    pub flags: u8,
}

impl MockFrame {
    fn press(&mut self, row: u8, col: u8) {
        self.pressed[row as usize] |= 1 << col;
    }
}

const PERIOD_MS: u64 = 16_000;

/// Keys tapped every 160 ms (pressed for 90 ms) during a scripted phase.
fn tap_sequence(frame: &mut MockFrame, t: u64, start: u64, keys: &[(u8, u8)]) {
    let since = t.saturating_sub(start);
    let i = (since / 160) as usize;
    if since % 160 < 90 {
        if let Some(&(r, c)) = keys.get(i % keys.len().max(1)) {
            frame.press(r, c);
        }
    }
}

/// Scripted state at `t` milliseconds into the loop.
pub fn frame_at(t_ms: u64) -> MockFrame {
    let t = t_ms % PERIOD_MS;
    let mut f = MockFrame::default();
    const FN1: (u8, u8) = (3, 4);
    const FN2: (u8, u8) = (7, 4);
    match t {
        // typing on BASE: "hello world"-ish positions
        0..=1999 => tap_sequence(&mut f, t, 0, &[(5, 5), (0, 3), (5, 2), (5, 2), (4, 2), (3, 5), (0, 2), (4, 2)]),
        // hold Fn1: SYM layer, type a few symbols and digits
        2000..=3999 => {
            f.layer_state = 1 << 1;
            f.press(FN1.0, FN1.1);
            tap_sequence(&mut f, t, 2200, &[(1, 1), (1, 2), (5, 4), (5, 3), (5, 2)]);
        }
        // hold Fn2: NAV layer, arrows
        4000..=5999 => {
            f.layer_state = 1 << 2;
            f.press(FN2.0, FN2.1);
            tap_sequence(&mut f, t, 4200, &[(5, 5), (5, 4), (5, 3), (5, 2)]);
        }
        // Fn1 + Fn2: tri-layer SYS
        6000..=7999 => {
            f.layer_state = (1 << 1) | (1 << 2) | (1 << 3);
            f.press(FN1.0, FN1.1);
            f.press(FN2.0, FN2.1);
            if (6800..7000).contains(&t) {
                f.press(0, 2); // F9
            }
        }
        // one-shot ACC: tap OSL(5), wait, then the vowel
        8000..=9499 => {
            if t < 8100 {
                f.press(5, 6); // OSL(5)
            }
            if t < 9000 {
                f.layer_state = 1 << 5;
                f.oneshot_layer = Some(5);
            } else if t < 9100 {
                f.press(0, 3); // è
            }
        }
        // mod-tap Ctrl held (LCTL_T(KC_TAB)) + C
        9500..=10999 => {
            f.press(1, 0);
            if t >= 9700 {
                f.mods = 0x01;
            }
            if (10_000..10_150).contains(&t) {
                f.press(2, 3);
            }
        }
        // Caps Word while typing
        11_000..=12_499 => {
            f.flags |= flags::CAPS_WORD;
            tap_sequence(&mut f, t, 11_000, &[(0, 1), (0, 2), (0, 3), (0, 4)]);
        }
        // DEBUG locked with TG(4), step over / step into
        12_500..=14_999 => {
            if t < 12_600 {
                f.press(0, 0);
            }
            f.layer_state = 1 << 4;
            f.flags |= flags::USER;
            tap_sequence(&mut f, t, 13_000, &[(1, 3), (1, 3), (1, 4), (1, 2)]);
        }
        _ => {}
    }
    f
}

pub struct MockKeyboard {
    start: Instant,
    keymap: Keymap,
    definition_xz: Vec<u8>,
    dynamic: Dynamic,
    pending: VecDeque<Report>,
    /// Fixed time for tests; `None` = real clock.
    frozen_ms: Option<u64>,
    /// Grows at every key written, like the firmware's `keymap_version`.
    keymap_version: u8,
    /// Speaks protocol 2 (`GET_MATRIX`) instead of 1.
    protocol_2: bool,
}

/// Macro buffer of the simulated keyboard: "hi" + Enter, then three empty macros.
const MOCK_MACROS: &[u8] = &[b'h', b'i', 1, 1, 0x28, 0, 0, 0, 0, 0, 0, 0];
/// QMK settings of the simulated keyboard (combo term, tapping term), 2 bytes each.
const MOCK_SETTINGS: &[(u16, u16)] = &[(2, 50), (7, 200)];

/// What the simulated keyboard has configured in Vial: the `.vil` entries
/// plus a few combos on the base layer, to show them off. Macros are not
/// dynamic entries: they come through VIA (`MOCK_MACROS`).
pub fn mock_dynamic() -> Dynamic {
    let vil = serde_json::from_str(MOCK_VIL).map(|v| Dynamic::from_vil(&v)).unwrap_or_default();
    let combo = |keys: [u16; 2], output| Combo { keys: [keys[0], keys[1], 0, 0], output };
    let combos = vec![
        combo([0x0D, 0x0E], 0x29),   // J + K = Esc
        combo([0x1B, 0x06], 0x0106), // X + C = Ctrl+C
        combo([0x07, 0x09], 0x2B),   // D + F = Tab
    ];
    Dynamic { combos, macros: Vec::new(), ..vil }
}

impl MockKeyboard {
    /// Starts the scripted loop `offset_ms` into it.
    pub fn starting_at(offset_ms: u64) -> Self {
        let mut m = Self::new();
        m.start = Instant::now().checked_sub(Duration::from_millis(offset_ms)).unwrap_or_else(Instant::now);
        m
    }

    pub fn new() -> Self {
        let keymap = Vil::parse(MOCK_VIL, 8, 7).map(|v| v.keymap).unwrap_or_else(|_| Keymap::new(6, 8, 7));
        let mut definition_xz = Vec::new();
        // compressing an in-memory buffer cannot fail
        let _ = lzma_rs::xz_compress(&mut MOCK_VIAL_JSON.as_bytes(), &mut definition_xz);
        MockKeyboard {
            start: Instant::now(),
            keymap,
            definition_xz,
            dynamic: mock_dynamic(),
            pending: VecDeque::new(),
            frozen_ms: None,
            keymap_version: 0,
            protocol_2: false,
        }
    }

    #[cfg(test)]
    pub fn frozen(ms: u64) -> Self {
        MockKeyboard { frozen_ms: Some(ms), ..Self::new() }
    }

    fn now_ms(&self) -> u64 {
        self.frozen_ms.unwrap_or_else(|| self.start.elapsed().as_millis() as u64)
    }

    fn answer(&mut self, req: &Report) -> Option<Report> {
        let mut r = *req;
        match req[0] {
            p::HUD_CMD => self.answer_hud(&mut r),
            p::VIAL_PREFIX => {
                r = [0u8; REPORT_SIZE];
                match req[1] {
                    p::VIAL_GET_KEYBOARD_ID => {
                        r[0..4].copy_from_slice(&6u32.to_le_bytes());
                        r[4..12].copy_from_slice(&MOCK_UID.to_le_bytes());
                        r[12] = 1;
                    }
                    p::VIAL_GET_SIZE => r[0..4].copy_from_slice(&(self.definition_xz.len() as u32).to_le_bytes()),
                    p::VIAL_GET_DEF => {
                        let start = u16::from_le_bytes([req[2], req[3]]) as usize * REPORT_SIZE;
                        if start >= self.definition_xz.len() {
                            return Some(*req); // out of range: echo, like vial.c
                        }
                        let end = (start + REPORT_SIZE).min(self.definition_xz.len());
                        r[..end - start].copy_from_slice(&self.definition_xz[start..end]);
                    }
                    p::VIAL_GET_ENCODER => r[..4].copy_from_slice(&[0x00, 0x81, 0x00, 0x80]), // Vol-, Vol+
                    p::VIAL_QMK_SETTINGS_QUERY => {
                        let after = u16::from_le_bytes([req[2], req[3]]);
                        r.fill(0xFF);
                        for (i, id) in MOCK_SETTINGS.iter().map(|s| s.0).filter(|&id| id > after).enumerate() {
                            r[i * 2..i * 2 + 2].copy_from_slice(&id.to_le_bytes());
                        }
                    }
                    p::VIAL_QMK_SETTINGS_GET => {
                        let id = u16::from_le_bytes([req[2], req[3]]);
                        r = *req;
                        match MOCK_SETTINGS.iter().find(|s| s.0 == id) {
                            Some(&(_, v)) => {
                                r[0] = 0;
                                r[1..3].copy_from_slice(&v.to_le_bytes());
                            }
                            None => r[0] = 0xFF,
                        }
                    }
                    p::VIAL_DYNAMIC_ENTRY_OP => {
                        let d = &self.dynamic;
                        let entry = match req[2] {
                            0x00 => {
                                r[..3].copy_from_slice(&[
                                    d.tap_dances.len() as u8,
                                    d.combos.len() as u8,
                                    d.key_overrides.len() as u8,
                                ]);
                                return Some(r);
                            }
                            0x01 => d.tap_dances.get(req[3] as usize).map(|e| e.to_bytes()),
                            0x03 => d.combos.get(req[3] as usize).map(|e| e.to_bytes()),
                            0x05 => d.key_overrides.get(req[3] as usize).map(|e| e.to_bytes()),
                            _ => return Some(*req),
                        };
                        match entry {
                            Some(e) => r[1..11].copy_from_slice(&e),
                            None => r[0] = 1,
                        }
                    }
                    _ => r = *req,
                }
            }
            p::VIA_GET_KEYBOARD_VALUE if req[1] == p::VIA_LAYOUT_OPTIONS => r[2..6].copy_from_slice(&[0; 4]),
            p::VIA_GET_LAYER_COUNT => r[1] = self.keymap.layers(),
            p::VIA_GET_PROTOCOL_VERSION => r[1..3].copy_from_slice(&9u16.to_be_bytes()),
            p::VIA_MACRO_COUNT => r[1] = 4,
            p::VIA_MACRO_BUFFER_SIZE => r[1..3].copy_from_slice(&(MOCK_MACROS.len() as u16).to_be_bytes()),
            p::VIA_MACRO_BUFFER => {
                let offset = u16::from_be_bytes([req[1], req[2]]) as usize;
                let n = (req[3] as usize).min(28);
                for i in 0..n {
                    r[4 + i] = MOCK_MACROS.get(offset + i).copied().unwrap_or(0);
                }
            }
            p::VIA_GET_KEYCODE => r[4..6].copy_from_slice(&self.keymap.get(req[1], req[2], req[3]).to_be_bytes()),
            p::VIA_SET_KEYCODE => {
                self.keymap.set(req[1], req[2], req[3], u16::from_be_bytes([req[4], req[5]]));
                self.keymap_version = self.keymap_version.wrapping_add(1);
            }
            0x12 => {
                let offset = u16::from_be_bytes([req[1], req[2]]) as usize;
                let size = req[3] as usize;
                if size <= 28 {
                    let per_layer = self.keymap.rows() as usize * self.keymap.cols() as usize;
                    for i in 0..size {
                        let byte = offset + i;
                        let (idx, low) = (byte / 2, byte % 2 == 1);
                        let (layer, pos) = (idx / per_layer, idx % per_layer);
                        let cols = self.keymap.cols() as usize;
                        let kc = self.keymap.get(layer as u8, (pos / cols) as u8, (pos % cols) as u8);
                        r[4 + i] = if low { kc as u8 } else { (kc >> 8) as u8 };
                    }
                }
            }
            _ => r[0] = p::ID_UNHANDLED,
        }
        Some(r)
    }

    fn answer_hud(&self, r: &mut Report) {
        let args = [r[4], r[5], r[6], r[7]];
        r[4..].fill(0);
        r[2] = p::STATUS_OK;
        match r[1] {
            0x01 => {
                r[4] = if self.protocol_2 { 2 } else { p::HUD_PROTOCOL_VERSION };
                r[5] = self.keymap.rows();
                r[6] = self.keymap.cols();
                r[7] = self.keymap.layers();
                r[8..10].copy_from_slice(&0x3Fu16.to_le_bytes());
                r[10..12].copy_from_slice(&0x7E12u16.to_le_bytes());
                r[12] = p::MAX_KEYMAP_CHUNK;
                r[14] = 1;
            }
            0x02 => {
                let f = frame_at(self.now_ms());
                r[4..8].copy_from_slice(&f.layer_state.to_le_bytes());
                r[8] = 1;
                r[10..18].copy_from_slice(&f.pressed);
                r[18] = f.mods;
                r[22] = f.oneshot_layer.unwrap_or(0xFF);
                r[23] = f.flags | flags::SLAVE_OK;
                r[24] = 2; // Windows
                r[25] = 0;
                r[26] = 1;
                r[28] = self.keymap_version;
            }
            0x04 if self.protocol_2 => r[4..12].copy_from_slice(&frame_at(self.now_ms()).pressed),
            0x03 => {
                let (layer, offset, count) = (args[0], u16::from_le_bytes([args[1], args[2]]), args[3]);
                r[4..8].copy_from_slice(&args);
                let total = self.keymap.rows() as u16 * self.keymap.cols() as u16;
                if layer >= self.keymap.layers()
                    || count == 0
                    || count > p::MAX_KEYMAP_CHUNK
                    || offset + count as u16 > total
                {
                    r[2] = 2;
                    return;
                }
                let cols = self.keymap.cols() as u16;
                for i in 0..count as u16 {
                    let pos = offset + i;
                    let kc = self.keymap.get(layer, (pos / cols) as u8, (pos % cols) as u8);
                    r[8 + 2 * i as usize..10 + 2 * i as usize].copy_from_slice(&kc.to_le_bytes());
                }
            }
            _ => r[2] = 1,
        }
    }
}

impl Default for MockKeyboard {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for MockKeyboard {
    fn send(&mut self, report: &Report) -> Result<(), TransportError> {
        if let Some(r) = self.answer(report) {
            self.pending.push_back(r);
        }
        Ok(())
    }

    fn recv(&mut self, _timeout: Duration) -> Result<Option<Report>, TransportError> {
        Ok(self.pending.pop_front())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hid::device::Device;

    fn device_at(ms: u64) -> Device {
        Device::new(Box::new(MockKeyboard::frozen(ms)), Duration::from_millis(50))
    }

    fn state_of(d: &mut Device) -> crate::hid::protocol::HudState {
        let info = d.info().unwrap();
        d.state(&info).unwrap()
    }

    /// Protocol 2: the pressed keys come from `GET_MATRIX`, GET_STATE has none that count.
    #[test]
    fn protocol_2_reads_the_matrix_separately() {
        let m = MockKeyboard { protocol_2: true, ..MockKeyboard::frozen(7000) };
        let mut d = Device::new(Box::new(m), Duration::from_millis(50));
        let info = d.info().unwrap();
        assert!(info.has_matrix_command() && info.row_bytes == 1);
        let s = d.state(&info).unwrap();
        assert!(s.is_pressed(3, 4) && s.is_pressed(7, 4));
        // the mock does not know GET_MATRIX as protocol 1
        assert!(
            MockKeyboard::frozen(0).answer(&p::hud_request(p::HudCommand::Matrix, 1, &[])).unwrap()[2] != p::STATUS_OK
        );
    }

    #[test]
    fn full_session_over_the_protocol() {
        let mut d = device_at(0);
        let info = d.info().unwrap();
        assert_eq!((info.rows, info.cols, info.layers, info.hud_tog_keycode), (8, 7, 6, 0x7E12));
        let size = d.definition_size().unwrap();
        let def = crate::vial::definition::Definition::from_xz(&d.definition_xz(size).unwrap()).unwrap();
        assert_eq!(def.visible_keys(d.layout_options().unwrap()).len(), 46);
        assert_eq!(d.vial_id().unwrap(), (6, MOCK_UID));
        let km = d.keymap(&info).unwrap();
        let expected = Vil::parse(MOCK_VIL, 8, 7).unwrap().keymap;
        assert_eq!(km, expected);
        assert_eq!(d.via_keymap(info.layers, info.rows, info.cols).unwrap(), expected);
        assert_eq!(d.layer_count().unwrap(), 6);
    }

    #[test]
    fn scripted_states() {
        assert_eq!(state_of(&mut device_at(3000)).layer_state, 1 << 1);
        let sys = state_of(&mut device_at(7000));
        assert_eq!(sys.layer_state, 0b1110);
        assert!(sys.is_pressed(3, 4) && sys.is_pressed(7, 4));
        let osl = state_of(&mut device_at(8500));
        assert_eq!(osl.oneshot_layer, Some(5));
        assert_eq!(state_of(&mut device_at(10_000)).mods, 0x01);
        assert!(state_of(&mut device_at(14_000)).flag(flags::USER));
        assert_eq!(state_of(&mut device_at(PERIOD_MS + 3000)).layer_state, 1 << 1);
    }

    #[test]
    fn dynamic_entries_over_the_protocol() {
        let mut d = Device::new(Box::new(MockKeyboard::frozen(0)), Duration::from_millis(50));
        assert_eq!(d.dynamic().unwrap(), mock_dynamic());
        assert_eq!(mock_dynamic().combos.len(), 3);
    }

    #[test]
    fn dump_over_the_protocol() {
        use crate::vial::dump::MacroAction;
        let def = crate::vial::definition::Definition::from_json_str(MOCK_VIAL_JSON).unwrap();
        let mut d = Device::new(Box::new(MockKeyboard::frozen(0)), Duration::from_millis(50));
        let dump = d.dump(&def).unwrap();
        assert_eq!(dump.keymap, Vil::parse(MOCK_VIL, 8, 7).unwrap().keymap);
        assert_eq!((dump.uid, dump.vial_protocol, dump.via_protocol), (MOCK_UID, 6, 9));
        assert_eq!(dump.macros.len(), 4);
        assert_eq!(dump.macros[0], vec![MacroAction::Text("hi".into()), MacroAction::Tap(vec![0x28])]);
        assert_eq!(dump.settings.get(&7), Some(&200));
        assert_eq!(dump.combos.len(), 3);
        // the definition's encoders, on every layer
        assert!(dump.encoders.iter().all(|l| l.iter().all(|e| *e == [0x81, 0x80])));
        // a .vil the HUD itself reads back
        let vil = Vil::parse(&dump.to_vil().to_string(), 8, 7).unwrap();
        assert_eq!(vil.keymap, dump.keymap);
        assert_eq!(vil.dynamic, Dynamic { macros: dump.macros.clone(), ..mock_dynamic() });
    }

    #[test]
    fn keys_written_over_the_protocol() {
        let mut d = device_at(0);
        let info = d.info().unwrap();
        let before = state_of(&mut d).keymap_version;
        assert_eq!(d.keycode(0, 1, 1).unwrap(), d.keymap(&info).unwrap().get(0, 1, 1));
        d.set_keycode(0, 1, 1, 0x29).unwrap();
        assert_eq!(d.keycode(0, 1, 1).unwrap(), 0x29);
        assert_eq!(d.keymap(&info).unwrap().get(0, 1, 1), 0x29);
        assert_ne!(state_of(&mut d).keymap_version, before);
    }

    #[test]
    fn foreign_answers_are_counted() {
        let mut m = MockKeyboard::frozen(0);
        // two answers meant for another program, already queued
        m.pending.push_back(p::via_request(&[0x12, 0, 0, 4]));
        m.pending.push_back(p::via_request(&[0xFE, 0x0D, 0x03]));
        let mut d = Device::new(Box::new(m), Duration::from_millis(50));
        state_of(&mut d);
        assert_eq!(d.take_foreign(), 2);
        assert_eq!(d.take_foreign(), 0);
        // our own answers are not foreign
        state_of(&mut d);
        d.info().unwrap();
        assert_eq!(d.take_foreign(), 0);
        // nothing sent, nothing received
        assert_eq!(d.listen(Duration::from_millis(5)).unwrap(), 0);
    }

    #[test]
    fn unknown_commands_are_unhandled() {
        let mut m = MockKeyboard::frozen(0);
        m.send(&p::via_request(&[0x42])).unwrap();
        assert_eq!(m.recv(Duration::ZERO).unwrap().unwrap()[0], p::ID_UNHANDLED);
    }
}
