//! Application model: effective keymap and transparent-key resolution.

use crate::hid::protocol::HudState;
use crate::vial::keycode::{decode, Keycode, LayerOp, KC_TRNS, TRI_LAYER_ADJUST, TRI_LAYER_LOWER, TRI_LAYER_UPPER};

/// Keycodes for every layer and matrix position (`layer × rows × cols`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    layers: u8,
    rows: u8,
    cols: u8,
    codes: Vec<u16>,
}

impl Keymap {
    pub fn new(layers: u8, rows: u8, cols: u8) -> Self {
        Keymap { layers, rows, cols, codes: vec![0; layers as usize * rows as usize * cols as usize] }
    }

    pub fn layers(&self) -> u8 {
        self.layers
    }

    pub fn rows(&self) -> u8 {
        self.rows
    }

    pub fn cols(&self) -> u8 {
        self.cols
    }

    fn index(&self, layer: u8, row: u8, col: u8) -> Option<usize> {
        (layer < self.layers && row < self.rows && col < self.cols)
            .then(|| (layer as usize * self.rows as usize + row as usize) * self.cols as usize + col as usize)
    }

    /// Keycode at a position; `KC_NO` when out of range.
    pub fn get(&self, layer: u8, row: u8, col: u8) -> u16 {
        self.index(layer, row, col).map(|i| self.codes[i]).unwrap_or(0)
    }

    pub fn set(&mut self, layer: u8, row: u8, col: u8, code: u16) {
        if let Some(i) = self.index(layer, row, col) {
            self.codes[i] = code;
        }
    }

    /// Writes keycodes of `layer` starting at `offset` (`row × cols + col`).
    pub fn set_run(&mut self, layer: u8, offset: u16, codes: &[u16]) {
        for (i, &code) in codes.iter().enumerate() {
            let pos = offset as usize + i;
            let (row, col) = (pos / self.cols as usize, pos % self.cols as usize);
            if row < self.rows as usize {
                self.set(layer, row as u8, col as u8, code);
            }
        }
    }

    /// Resolves `KC_TRNS` like QMK does: starting from `layer`, walk down
    /// the layers that are active (`active_mask`) until a non-transparent
    /// keycode is found. Returns the keycode and the layer it comes from.
    pub fn resolve(&self, layer: u8, active_mask: u32, row: u8, col: u8) -> (u16, u8) {
        let top = layer.min(self.layers.saturating_sub(1));
        for l in (0..=top).rev() {
            if l != top && active_mask & (1 << l) == 0 {
                continue;
            }
            let code = self.get(l, row, col);
            if code != KC_TRNS {
                return (code, l);
            }
        }
        (KC_TRNS, 0)
    }
}

/// Why a layer is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerHold {
    /// A layer key is being held (MO, TL_LOWR/UPPR, LT, tri-layer...).
    Held,
    /// Toggled (TG, TO) or locked with Layer Lock: stays on after release.
    Locked,
    /// One-shot (OSL): waiting for the next key.
    OneShot,
}

/// Classifies the active layers of `state` (default layer excluded), highest first.
///
/// The firmware reports which layers are on and which are locked with Layer
/// Lock (`locked_layers`, layers 0-7), not why the others are on: a layer
/// counts as held when one of the pressed keys is a momentary key for it
/// (unless Layer Lock keeps it on: it stays after the release); the
/// tri-layer adjust layer is held when both its lower and upper layers are
/// held; anything else that is on without a pending one-shot is locked
/// (TG, TO).
pub fn classify_layers(keymap: &Keymap, state: &HudState) -> Vec<(u8, LayerHold)> {
    let mask = state.active_mask();
    let top = highest_layer(mask);
    let mut held = 0u32;
    for (row, col) in state.matrix.pressed() {
        if row >= keymap.rows() || col >= keymap.cols() {
            continue;
        }
        let (code, _) = keymap.resolve(top, mask, row, col);
        let target = match decode(code) {
            Keycode::Layer {
                op:
                    LayerOp::Momentary
                    | LayerOp::TriLayerLower
                    | LayerOp::TriLayerUpper
                    | LayerOp::TapToggle
                    | LayerOp::OneShot,
                layer,
            } => Some(layer),
            Keycode::LayerTap { layer, .. } | Keycode::LayerMod { layer, .. } => Some(layer),
            _ => None,
        };
        if let Some(l) = target.filter(|l| *l < 32) {
            held |= 1 << l;
        }
    }
    let lower = 1u32 << TRI_LAYER_LOWER;
    let upper = 1u32 << TRI_LAYER_UPPER;
    if held & lower != 0 && held & upper != 0 {
        held |= 1 << TRI_LAYER_ADJUST;
    }

    (0..32u8)
        .rev()
        .filter(|l| state.layer_state & (1 << l) != 0)
        .map(|l| {
            let layer_locked = l < 8 && state.locked_layers & (1 << l) != 0;
            let how = if layer_locked {
                LayerHold::Locked
            } else if held & (1 << l) != 0 {
                LayerHold::Held
            } else if state.oneshot_layer == Some(l) {
                LayerHold::OneShot
            } else {
                LayerHold::Locked
            };
            (l, how)
        })
        .collect()
}

/// How a modifier is active, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ModLevel {
    Off,
    /// Weak mods: added by a key for its own duration (e.g. `C(KC_Z)`, Caps Word).
    Weak,
    /// One-shot mod waiting for the next key.
    OneShot,
    /// One-shot mod locked (tapped repeatedly).
    OneShotLocked,
    /// Physically held.
    Held,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    Ctrl,
    Shift,
    Alt,
    Gui,
}

pub const MODIFIERS: [Modifier; 4] = [Modifier::Ctrl, Modifier::Shift, Modifier::Alt, Modifier::Gui];

/// Level of `m` from the 8-bit QMK mod masks (bits 0-3 left, 4-7 right).
pub fn mod_level(state: &HudState, m: Modifier) -> ModLevel {
    let bit = match m {
        Modifier::Ctrl => 0,
        Modifier::Shift => 1,
        Modifier::Alt => 2,
        Modifier::Gui => 3,
    };
    let on = |mask: u8| mask & (1 << bit) != 0 || mask & (1 << (bit + 4)) != 0;
    if on(state.mods) {
        ModLevel::Held
    } else if on(state.oneshot_locked_mods) {
        ModLevel::OneShotLocked
    } else if on(state.oneshot_mods) {
        ModLevel::OneShot
    } else if on(state.weak_mods) {
        ModLevel::Weak
    } else {
        ModLevel::Off
    }
}

/// Highest active layer, as `get_highest_layer(layer_state | default_layer_state)`.
pub fn highest_layer(mask: u32) -> u8 {
    if mask == 0 {
        0
    } else {
        31 - mask.leading_zeros() as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_set_bounds() {
        let mut k = Keymap::new(2, 2, 3);
        k.set(1, 1, 2, 42);
        assert_eq!(k.get(1, 1, 2), 42);
        k.set(2, 0, 0, 1); // ignored
        k.set(0, 5, 0, 1); // ignored
        assert_eq!(k.get(9, 9, 9), 0);
        k.set_run(0, 2, &[7, 8, 9, 10, 11, 12]); // spills over rows, last one dropped
        assert_eq!((k.get(0, 0, 2), k.get(0, 1, 0), k.get(0, 1, 2)), (7, 8, 10));
    }

    #[test]
    fn transparent_resolution_follows_active_layers() {
        // layers: 0 base, 1 (TRNS at 0,0), 2 (TRNS at 0,0)
        let mut k = Keymap::new(3, 1, 1);
        k.set(0, 0, 0, 0x04);
        k.set(1, 0, 0, 0x05);
        k.set(2, 0, 0, KC_TRNS);
        // layer 1 inactive: layer 2 falls through to base
        assert_eq!(k.resolve(2, 0b101, 0, 0), (0x04, 0));
        // layer 1 active too
        assert_eq!(k.resolve(2, 0b111, 0, 0), (0x05, 1));
        // all transparent
        k.set(0, 0, 0, KC_TRNS);
        k.set(1, 0, 0, KC_TRNS);
        assert_eq!(k.resolve(2, 0b111, 0, 0), (KC_TRNS, 0));
        // out of range layer is clamped
        assert_eq!(k.resolve(9, 0, 0, 0).0, KC_TRNS);
    }

    fn corne() -> Keymap {
        crate::vial::vil::Vil::parse(crate::hid::mock::MOCK_VIL, 8, 7).unwrap().keymap
    }

    fn state(layers: u32, pressed: &[(u8, u8)]) -> HudState {
        let mut s = HudState { layer_state: layers, default_layer_state: 1, ..Default::default() };
        for &(r, c) in pressed {
            s.matrix.set(r, c);
        }
        s
    }

    #[test]
    fn classifies_held_locked_and_oneshot_layers() {
        let k = corne();
        // Fn1 (TL_LOWR at 3,4) held
        assert_eq!(classify_layers(&k, &state(0b10, &[(3, 4)])), vec![(1, LayerHold::Held)]);
        // Fn1 + Fn2: tri-layer SYS is held too
        let tri = classify_layers(&k, &state(0b1110, &[(3, 4), (7, 4)]));
        assert_eq!(tri, vec![(3, LayerHold::Held), (2, LayerHold::Held), (1, LayerHold::Held)]);
        // DEBUG toggled with TG(4), nothing held
        assert_eq!(classify_layers(&k, &state(1 << 4, &[])), vec![(4, LayerHold::Locked)]);
        // DEBUG locked while holding Fn1: SYM held, DEBUG still locked
        let both = classify_layers(&k, &state((1 << 4) | 0b10, &[(3, 4)]));
        assert_eq!(both, vec![(4, LayerHold::Locked), (1, LayerHold::Held)]);
        // one-shot ACC waiting
        let mut s = state(1 << 5, &[]);
        s.oneshot_layer = Some(5);
        assert_eq!(classify_layers(&k, &s), vec![(5, LayerHold::OneShot)]);
        // Layer Lock while Fn1 is still held: the firmware says locked
        let mut s = state(0b10, &[(3, 4)]);
        s.locked_layers = 0b10;
        assert_eq!(classify_layers(&k, &s), vec![(1, LayerHold::Locked)]);
        // base only
        assert!(classify_layers(&k, &state(0, &[(0, 1)])).is_empty());
    }

    #[test]
    fn modifier_levels() {
        let mut s = HudState::default();
        assert_eq!(mod_level(&s, Modifier::Ctrl), ModLevel::Off);
        s.weak_mods = 0x02;
        assert_eq!(mod_level(&s, Modifier::Shift), ModLevel::Weak);
        s.oneshot_mods = 0x20; // right shift one-shot
        assert_eq!(mod_level(&s, Modifier::Shift), ModLevel::OneShot);
        s.oneshot_locked_mods = 0x02;
        assert_eq!(mod_level(&s, Modifier::Shift), ModLevel::OneShotLocked);
        s.mods = 0x40 | 0x01; // RAlt + LCtl held
        assert_eq!(mod_level(&s, Modifier::Alt), ModLevel::Held);
        assert_eq!(mod_level(&s, Modifier::Ctrl), ModLevel::Held);
        assert_eq!(mod_level(&s, Modifier::Gui), ModLevel::Off);
    }

    #[test]
    fn highest() {
        assert_eq!(highest_layer(0), 0);
        assert_eq!(highest_layer(0b1), 0);
        assert_eq!(highest_layer(0b1_0110), 4);
        assert_eq!(highest_layer(1 << 31), 31);
    }
}
