//! Human-friendly legends for keycodes (what is drawn on each key).

use super::definition::CustomKeycode;
use super::dump::MacroAction;
use super::dynamic::TapDance;
use super::keycode::{decode, qmk_name, Keycode, LayerOp, Mods5};
use super::names;
use crate::i18n::pick;
use crate::tr_format;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegendKind {
    Normal,
    /// `KC_TRNS`: the renderer shows the legend of the first non-transparent layer below.
    Transparent,
    /// `KC_NO`: empty, dark key.
    Empty,
    /// Layer key (`MO`, `TG`, `OSL`, `TL_LOWR`...): drawn with the target layer color.
    Layer(u8),
    Modifier,
    Custom,
}

/// Text alternatives: the renderer uses `symbol` when the font has all its
/// glyphs, otherwise `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Text {
    pub symbol: String,
    pub text: String,
}

impl Text {
    fn plain(s: impl Into<String>) -> Self {
        let s = s.into();
        Text { symbol: s.clone(), text: s }
    }

    fn sym(symbol: &str, text: &str) -> Self {
        Text { symbol: symbol.into(), text: text.into() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Legend {
    /// Main legend (tap action).
    pub tap: Text,
    /// Small secondary legend (hold action of mod-tap / layer-tap).
    pub hold: Option<Text>,
    pub tooltip: String,
    pub kind: LegendKind,
}

pub struct LegendContext<'a> {
    pub layer_names: &'a [String],
    pub custom: &'a [CustomKeycode],
    /// Indexed by tap dance number.
    pub tap_dances: &'a [TapDance],
    /// Indexed by macro number.
    pub macros: &'a [Vec<MacroAction>],
}

impl LegendContext<'_> {
    fn layer_name(&self, layer: u8) -> String {
        self.layer_names.get(layer as usize).cloned().unwrap_or_else(|| format!("L{layer}"))
    }
}

/// Compact symbols for common keys, by canonical QMK key name.
fn compact(key: &str) -> Option<Text> {
    let t = |s: &str, x: &str| Some(Text::sym(s, x));
    let p = |s: &str| Some(Text::plain(s));
    match key {
        "KC_BACKSPACE" => t("⌫", "Bksp"),
        "KC_ENTER" => t("⏎", "Enter"),
        "KC_TAB" => t("⇥", "Tab"),
        "KC_ESCAPE" => t("⎋", "Esc"),
        "KC_SPACE" => t("␣", "Space"),
        "KC_DELETE" => t("⌦", "Del"),
        "KC_LEFT" => t("←", "Left"),
        "KC_RIGHT" => t("→", "Right"),
        "KC_UP" => t("↑", "Up"),
        "KC_DOWN" => t("↓", "Down"),
        "KC_LEFT_SHIFT" | "KC_RIGHT_SHIFT" => t("⇧", "Shift"),
        "KC_CAPS_LOCK" => t("⇪", "Caps"),
        "KC_LEFT_CTRL" | "KC_RIGHT_CTRL" => p("Ctrl"),
        "KC_LEFT_ALT" => p("Alt"),
        "KC_RIGHT_ALT" => p("AltGr"),
        "KC_LEFT_GUI" | "KC_RIGHT_GUI" => p("Gui"),
        "KC_PAGE_UP" => p("PgUp"),
        "KC_PAGE_DOWN" => p("PgDn"),
        "KC_HOME" => p("Home"),
        "KC_END" => p("End"),
        "KC_INSERT" => p("Ins"),
        "KC_PRINT_SCREEN" => p("PrtSc"),
        "KC_NUM_LOCK" => p("NumLk"),
        "KC_SCROLL_LOCK" => p("ScrLk"),
        "KC_PAUSE" => p("Pause"),
        "KC_APPLICATION" => p("Menu"),
        "KC_AUDIO_MUTE" => p("Mute"),
        "KC_AUDIO_VOL_UP" | "KC_KB_VOLUME_UP" => p("Vol+"),
        "KC_AUDIO_VOL_DOWN" | "KC_KB_VOLUME_DOWN" => p("Vol−"),
        "KC_MEDIA_PLAY_PAUSE" => t("⏯", "Play"),
        "KC_MEDIA_STOP" => t("⏹", "Stop"),
        "KC_MEDIA_NEXT_TRACK" => t("⏭", "Next"),
        "KC_MEDIA_PREV_TRACK" => t("⏮", "Prev"),
        "KC_BRIGHTNESS_UP" => p("Bri+"),
        "KC_BRIGHTNESS_DOWN" => p("Bri−"),
        "QK_BOOTLOADER" => p("Boot"),
        "QK_REBOOT" => p("Reboot"),
        "QK_CLEAR_EEPROM" => p("ClrEE"),
        "QK_REPEAT_KEY" => t("⟳", "Rep"),
        "QK_ALT_REPEAT_KEY" => p("AltRep"),
        "QK_CAPS_WORD_TOGGLE" => p("CapsW"),
        "QK_LAYER_LOCK" => t("🔒", "LLock"),
        "QK_UNDERGLOW_TOGGLE" | "QK_RGB_MATRIX_TOGGLE" => p("RGB"),
        "QK_UNDERGLOW_MODE_NEXT" | "QK_RGB_MATRIX_MODE_NEXT" => p("RGB→"),
        "QK_UNDERGLOW_MODE_PREVIOUS" | "QK_RGB_MATRIX_MODE_PREVIOUS" => p("RGB←"),
        "QK_UNDERGLOW_VALUE_UP" | "QK_RGB_MATRIX_VALUE_UP" => p("Val+"),
        "QK_UNDERGLOW_VALUE_DOWN" | "QK_RGB_MATRIX_VALUE_DOWN" => p("Val−"),
        "QK_UNDERGLOW_HUE_UP" | "QK_RGB_MATRIX_HUE_UP" => p("Hue+"),
        "QK_UNDERGLOW_HUE_DOWN" | "QK_RGB_MATRIX_HUE_DOWN" => p("Hue−"),
        "QK_UNDERGLOW_SATURATION_UP" | "QK_RGB_MATRIX_SATURATION_UP" => p("Sat+"),
        "QK_UNDERGLOW_SATURATION_DOWN" | "QK_RGB_MATRIX_SATURATION_DOWN" => p("Sat−"),
        _ => None,
    }
}

/// Character produced by Shift + basic key, on the layout the system types
/// with (Italian or US). Keycodes are the
/// US positions, as the host sees them.
pub fn shifted(kc: u8) -> Option<&'static str> {
    shifted_in(crate::os_layout::italian(), kc)
}

fn shifted_in(italian: bool, kc: u8) -> Option<&'static str> {
    if italian {
        return Some(match kc {
            0x1E => "!",
            0x1F => "\"",
            0x20 => "£",
            0x21 => "$",
            0x22 => "%",
            0x23 => "&",
            0x24 => "/",
            0x25 => "(",
            0x26 => ")",
            0x27 => "=",
            0x2D => "?",
            0x2E => "^",
            0x2F => "é",
            0x30 => "*",
            0x31 | 0x32 => "§",
            0x33 => "ç",
            0x34 => "°",
            0x35 => "|",
            0x36 => ";",
            0x37 => ":",
            0x38 => "_",
            0x64 => ">",
            _ => return None,
        });
    }
    Some(match kc {
        0x1E => "!",
        0x1F => "@",
        0x20 => "#",
        0x21 => "$",
        0x22 => "%",
        0x23 => "^",
        0x24 => "&",
        0x25 => "*",
        0x26 => "(",
        0x27 => ")",
        0x2D => "_",
        0x2E => "+",
        0x2F => "{",
        0x30 => "}",
        0x31 => "|",
        0x33 => ":",
        0x34 => "\"",
        0x35 => "~",
        0x36 => "<",
        0x37 => ">",
        0x38 => "?",
        _ => return None,
    })
}

fn is_modifier(code: u16) -> bool {
    (0xE0..=0xE7).contains(&code)
}

/// Legend of a plain (non-composite) keycode.
fn basic_text(code: u16) -> (Text, String) {
    match names::qmk_by_code(code) {
        Some(e) => {
            let tooltip = if e.label.is_empty() || e.label == e.key {
                e.short_name().to_string()
            } else {
                format!("{} ({})", e.label, e.short_name())
            };
            let text = compact(e.key).unwrap_or_else(|| {
                if !e.label.is_empty() && e.label.chars().count() <= 4 {
                    Text::plain(e.label)
                } else {
                    let n = e.short_name();
                    Text::plain(n.strip_prefix("KC_").or_else(|| n.strip_prefix("QK_")).unwrap_or(n))
                }
            });
            (text, tooltip)
        }
        None => (Text::plain(format!("{code:#06x}")), format!("{code:#06x}")),
    }
}

fn basic_symbol(kc: u8) -> Text {
    basic_text(kc as u16).0
}

fn layer_legend(op: LayerOp, layer: u8, ctx: &LegendContext) -> (String, String) {
    let name = ctx.layer_name(layer);
    let (short, what) = match op {
        LayerOp::Momentary | LayerOp::TriLayerLower | LayerOp::TriLayerUpper => {
            (name.clone(), pick("tieni premuto", "hold"))
        }
        LayerOp::Toggle => (format!("⇄ {name}"), pick("attiva/disattiva", "toggle")),
        LayerOp::OneShot => (format!("1×{name}"), "one-shot"),
        LayerOp::To => (format!("→{name}"), pick("passa al layer", "switch to the layer")),
        LayerOp::DefaultLayer | LayerOp::PersistentDefault => (format!("⌂{name}"), pick("layer di base", "base layer")),
        LayerOp::TapToggle => (format!("TT {name}"), pick("tieni o tocca", "hold or tap")),
    };
    (short, format!("{}: layer {layer} {name} ({what})", op.prefix()))
}

/// Tap dance: the tap action large, the hold action small (like a mod-tap),
/// every action in the tooltip.
fn tap_dance_legend(n: u8, ctx: &LegendContext) -> Legend {
    let Some(td) = ctx.tap_dances.get(n as usize).filter(|t| !t.is_empty()) else {
        return Legend {
            tap: Text::plain(format!("TD{n}")),
            hold: None,
            tooltip: format!("Tap dance {n}"),
            kind: LegendKind::Normal,
        };
    };
    // nested tap dances would recurse: show them by number
    let part = |code: u16| match decode(code) {
        Keycode::TapDance(m) => Text::plain(format!("TD{m}")),
        _ => legend(code, ctx).tap,
    };
    let mut lines = vec![format!("Tap dance {n}")];
    for (action, code) in td.actions() {
        if code != 0 {
            lines.push(format!("{}: {}", action.describe(), part(code).text));
        }
    }
    if td.term_ms != 0 {
        lines.push(format!("tapping term {} ms", td.term_ms));
    }
    let tap = if td.on_tap != 0 { part(td.on_tap) } else { Text::plain(format!("TD{n}")) };
    // the second action shown: hold, else double tap
    let hold = [td.on_hold, td.on_double_tap].into_iter().find(|&c| c != 0).map(part);
    Legend { tap, hold, tooltip: lines.join("\n"), kind: LegendKind::Normal }
}

/// Longest macro text on the key itself.
const MACRO_KEY_CHARS: usize = 6;

/// Macro: what it types or presses, shortened on the key ("hi⏎", "Ctrl+C"),
/// every step in the tooltip.
fn macro_legend(n: u8, ctx: &LegendContext) -> Legend {
    let empty = Legend {
        tap: Text::plain(format!("M{n}")),
        hold: None,
        tooltip: format!("Macro {n}"),
        kind: LegendKind::Normal,
    };
    let Some(steps) = ctx.macros.get(n as usize).filter(|m| !m.is_empty()) else { return empty };
    // nested macros would recurse: show them by number
    let key = |code: u16| match decode(code) {
        Keycode::Macro(m) => Text::plain(format!("M{m}")),
        _ => legend(code, ctx).tap,
    };
    let (mut symbol, mut text) = (String::new(), String::new());
    let mut lines = vec![format!("Macro {n}")];
    for step in steps {
        let names = |codes: &[u16], sep: &str| codes.iter().map(|&c| key(c).text).collect::<Vec<_>>().join(sep);
        match step {
            MacroAction::Text(t) => {
                symbol.push_str(t);
                text.push_str(t);
                lines.push(tr_format!("scrive «{t}»", "types “{t}”"));
            }
            MacroAction::Tap(codes) => {
                for &c in codes {
                    let k = key(c);
                    symbol.push_str(&k.symbol);
                    text.push_str(&k.text);
                }
                lines.push(tr_format!("tocca {}", "taps {}", names(codes, " ")));
            }
            MacroAction::Down(codes) => {
                for &c in codes {
                    let k = key(c);
                    symbol.push_str(&format!("{}+", k.symbol));
                    text.push_str(&format!("{}+", k.text));
                }
                lines.push(tr_format!("preme {}", "presses {}", names(codes, " ")));
            }
            MacroAction::Up(codes) => lines.push(tr_format!("rilascia {}", "releases {}", names(codes, " "))),
            MacroAction::Delay(ms) => lines.push(tr_format!("attende {ms} ms", "waits {ms} ms")),
        }
    }
    let short = |s: &str| {
        let s = s.trim_end_matches('+');
        if s.chars().count() > MACRO_KEY_CHARS {
            format!("{}…", s.chars().take(MACRO_KEY_CHARS - 1).collect::<String>())
        } else {
            s.to_string()
        }
    };
    if text.is_empty() {
        return Legend { tooltip: lines.join("\n"), ..empty };
    }
    Legend {
        tap: Text { symbol: short(&symbol), text: short(&text) },
        hold: Some(Text::plain(format!("M{n}"))),
        tooltip: lines.join("\n"),
        kind: LegendKind::Normal,
    }
}

/// Builds the legend for `code`.
pub fn legend(code: u16, ctx: &LegendContext) -> Legend {
    let normal = |tap: Text, tooltip: String| Legend { tap, hold: None, tooltip, kind: LegendKind::Normal };
    match decode(code) {
        Keycode::No => Legend { tap: Text::plain(""), hold: None, tooltip: "KC_NO".into(), kind: LegendKind::Empty },
        Keycode::Transparent => {
            Legend { tap: Text::plain(""), hold: None, tooltip: "KC_TRNS".into(), kind: LegendKind::Transparent }
        }
        Keycode::Basic(c) => {
            let (tap, tooltip) = basic_text(c);
            let kind = if is_modifier(c) { LegendKind::Modifier } else { LegendKind::Normal };
            Legend { tap, hold: None, tooltip, kind }
        }
        Keycode::Mods { mods, kc } => {
            if mods == Mods5(0x02) || mods == Mods5(0x12) {
                if let Some(ch) = shifted(kc) {
                    return normal(Text::plain(ch), format!("{ch} ({})", qmk_name(code)));
                }
            }
            let key = basic_symbol(kc);
            let tap = Text {
                symbol: format!("{}+{}", mods.display(), key.symbol),
                text: format!("{}+{}", mods.display(), key.text),
            };
            normal(tap, qmk_name(code))
        }
        Keycode::ModTap { mods, kc } => Legend {
            tap: basic_symbol(kc),
            hold: Some(Text::plain(mods.display())),
            tooltip: tr_format!(
                "{}: tocca {}, tieni {}",
                "{}: tap {}, hold {}",
                qmk_name(code),
                basic_symbol(kc).text,
                mods.display()
            ),
            kind: LegendKind::Normal,
        },
        Keycode::LayerTap { layer, kc } => Legend {
            tap: basic_symbol(kc),
            hold: Some(Text::plain(ctx.layer_name(layer))),
            tooltip: tr_format!(
                "{}: tocca {}, tieni layer {}",
                "{}: tap {}, hold layer {}",
                qmk_name(code),
                basic_symbol(kc).text,
                ctx.layer_name(layer)
            ),
            kind: LegendKind::Layer(layer),
        },
        Keycode::LayerMod { layer, mods } => Legend {
            tap: Text::plain(ctx.layer_name(layer)),
            hold: Some(Text::plain(mods.display())),
            tooltip: qmk_name(code),
            kind: LegendKind::Layer(layer),
        },
        Keycode::Layer { op, layer } => {
            let (short, tooltip) = layer_legend(op, layer, ctx);
            Legend { tap: Text::plain(short), hold: None, tooltip, kind: LegendKind::Layer(layer) }
        }
        Keycode::OneShotMod(m) => normal(Text::plain(format!("1×{}", m.display())), qmk_name(code)),
        Keycode::TapDance(n) => tap_dance_legend(n, ctx),
        Keycode::Macro(n) => macro_legend(n, ctx),
        Keycode::Custom(n) => match ctx.custom.get(n as usize) {
            Some(c) => Legend {
                tap: Text::plain(c.short_name.clone()),
                hold: None,
                tooltip: c.title.clone(),
                kind: LegendKind::Custom,
            },
            None => Legend {
                tap: Text::plain(format!("USER{n:02}")),
                hold: None,
                tooltip: tr_format!("Tasto personalizzato {n}", "Custom key {n}"),
                kind: LegendKind::Custom,
            },
        },
        Keycode::Unknown(c) => normal(
            Text::plain(format!("{c:04X}")),
            tr_format!("Keycode sconosciuto {c:#06x}", "Unknown keycode {c:#06x}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with<'a>(names: &'a [String], custom: &'a [CustomKeycode]) -> LegendContext<'a> {
        LegendContext { layer_names: names, custom, tap_dances: &[], macros: &[] }
    }

    fn names() -> Vec<String> {
        ["BASE", "SYM", "NAV", "SYS", "DEBUG", "ACC"].map(String::from).to_vec()
    }

    #[test]
    fn mod_tap_has_tap_and_hold() {
        let n = names();
        let l = legend(0x212B, &ctx_with(&n, &[]));
        assert_eq!(l.tap.symbol, "⇥");
        assert_eq!(l.tap.text, "Tab");
        assert_eq!(l.hold, Some(Text::plain("Ctrl")));
    }

    #[test]
    fn combined_mods_are_readable() {
        let n = names();
        assert_eq!(legend(0x011D, &ctx_with(&n, &[])).tap.text, "Ctrl+Z");
        assert_eq!(legend(0x033E, &ctx_with(&n, &[])).tap.text, "Ctrl+Shift+F5");
        assert_eq!(legend(0x021E, &ctx_with(&n, &[])).tap.text, "!");
        assert_eq!(legend(0x0445, &ctx_with(&n, &[])).tap.text, "Alt+F12");
    }

    #[test]
    fn layer_keys_name_the_target() {
        let n = names();
        let l = legend(0x7C77, &ctx_with(&n, &[]));
        assert_eq!((l.tap.text.as_str(), l.kind), ("SYM", LegendKind::Layer(1)));
        assert_eq!(legend(0x5264, &ctx_with(&n, &[])).kind, LegendKind::Layer(4));
        assert_eq!(legend(0x5285, &ctx_with(&n, &[])).tap.text, "1×ACC");
    }

    #[test]
    fn custom_keycodes_use_short_name() {
        let n = names();
        let custom = vec![CustomKeycode { name: "à".into(), title: "Accentata à".into(), short_name: "à".into() }];
        let l = legend(0x7E00, &ctx_with(&n, &custom));
        assert_eq!((l.tap.text.as_str(), l.tooltip.as_str(), l.kind), ("à", "Accentata à", LegendKind::Custom));
        assert_eq!(legend(0x7E05, &ctx_with(&n, &custom)).tap.text, "USER05");
    }

    #[test]
    fn special_kinds() {
        let n = names();
        assert_eq!(legend(0, &ctx_with(&n, &[])).kind, LegendKind::Empty);
        assert_eq!(legend(1, &ctx_with(&n, &[])).kind, LegendKind::Transparent);
        assert_eq!(legend(0xE1, &ctx_with(&n, &[])).kind, LegendKind::Modifier);
        assert_eq!(legend(0x04, &ctx_with(&n, &[])).tap.text, "A");
        assert_eq!(legend(0x33, &ctx_with(&n, &[])).tap.text, ";");
    }

    #[test]
    fn tap_dance_shows_its_actions() {
        let n = names();
        let tds = [TapDance { on_tap: 0x04, on_hold: 0xE0, on_double_tap: 0x29, on_tap_hold: 0, term_ms: 0 }];
        let ctx = LegendContext { layer_names: &n, custom: &[], tap_dances: &tds, macros: &[] };
        let l = legend(0x5700, &ctx);
        assert_eq!((l.tap.text.as_str(), l.hold.map(|h| h.text)), ("A", Some("Ctrl".into())));
        assert_eq!(l.tooltip, "Tap dance 0\ntap: A\nhold: Ctrl\ndouble tap: Esc");
        // unknown or empty: by number
        assert_eq!(legend(0x5701, &ctx).tap.text, "TD1");
    }

    #[test]
    fn macros_show_what_they_do() {
        let n = names();
        let macros = [
            vec![MacroAction::Text("hi".into()), MacroAction::Tap(vec![0x28])],
            vec![MacroAction::Down(vec![0xE0]), MacroAction::Tap(vec![0x06]), MacroAction::Up(vec![0xE0])],
            vec![MacroAction::Text("buongiorno".into()), MacroAction::Delay(50)],
            vec![],
        ];
        let ctx = LegendContext { layer_names: &n, custom: &[], tap_dances: &[], macros: &macros };
        let m = |i: u16| legend(0x7700 + i, &ctx);
        assert_eq!((m(0).tap.symbol.as_str(), m(0).hold.map(|h| h.text)), ("hi⏎", Some("M0".into())));
        assert_eq!(m(0).tap.text, "hiEnt…");
        assert_eq!(m(0).tooltip, "Macro 0\ntypes “hi”\ntaps Enter");
        assert_eq!(m(1).tap.text, "Ctrl+C");
        assert_eq!(m(1).tooltip, "Macro 1\npresses Ctrl\ntaps C\nreleases Ctrl");
        assert_eq!(m(2).tap.text, "buong…");
        assert!(m(2).tooltip.ends_with("waits 50 ms"));
        // empty or unknown: by number
        assert_eq!((m(3).tap.text.as_str(), m(3).hold), ("M3", None));
        assert_eq!(m(9).tap.text, "M9");
    }

    #[test]
    fn shift_follows_the_layout() {
        // ; and ' keys: US layout vs Italian
        assert_eq!(shifted_in(false, 0x33), Some(":"));
        assert_eq!(shifted_in(true, 0x37), Some(":"));
        assert_eq!(shifted_in(false, 0x1F), Some("@"));
        assert_eq!(shifted_in(true, 0x1F), Some("\""));
        assert_eq!(shifted_in(false, 0x04), None); // letters are not symbols
    }

    #[test]
    fn legends_never_panic() {
        let n = names();
        for code in 0..=u16::MAX {
            let _ = legend(code, &ctx_with(&n, &[]));
        }
    }
}
