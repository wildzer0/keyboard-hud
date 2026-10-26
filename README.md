# <img src="app/packaging/icon/icon.png" width="40" align="top" alt=""> Keyboard HUD

An always-on-top HUD that shows your **QMK keyboard with Vial** and updates it in real time: the active layer with its legends, pressed keys, modifiers, one-shots, Caps Lock / Caps Word, and the host OS.

![Adjust layer with Lower + Raise held](docs/img/tri-layer.png)

- **Portable**: one executable per system, no installation and no administrator rights. It also runs from a USB stick.
- **The keyboard is the source of truth**: the HUD queries the firmware over Raw HID, the same interface Vial uses. No system hooks, no keylogger, no Accessibility permission. It works on Wayland too.
- **No text is recorded**: the HUD only receives which physical keys are pressed and on which layer, never the characters.

| | |
|---|---|
| ![Locked layer with custom labels](docs/img/debug.png) | ![Searching for "(": Lower + D](docs/img/search.png) |
| A locked layer, with custom labels for VS Code | Search: `(` is on the Lower layer, with **Lower + D** |
| ![Heatmap](docs/img/heatmap.png) | ![Offline mode](docs/img/offline.png) |
| Key press heatmap | Offline mode (vial.json + .vil) |

The screenshots show the simulated keyboard (`--mock`), a Corne.

---

## Contents

1. [Supported keyboards](#supported-keyboards)
2. [Download](#download)
3. [First start](#first-start)
4. [Firmware](#firmware)
5. [Usage](#usage)
6. [Linux: udev rule](#linux-udev-rule)
7. [Windows: SmartScreen](#windows-smartscreen)
8. [Wayland](#wayland)
9. [Living with Vial](#living-with-vial)
10. [Configuration](#configuration)
11. [Styles](#styles)
12. [Automatic backup](#automatic-backup)
13. [Statistics and privacy](#statistics-and-privacy)
14. [Offline and simulated modes](#offline-and-simulated-modes)
15. [Command line options](#command-line-options)
16. [Building the app](#building-the-app)
17. [Troubleshooting](#troubleshooting)
18. [License and credits](#license-and-credits)

---

## Supported keyboards

The HUD reads the layout, the keymap, macros, combos, tap dances and key overrides from the keyboard's own Vial definition, so it is not tied to a model:

- **Any keyboard with a Vial firmware** works, with a static view: the layers read through VIA, browsable with the mouse wheel, without live state.
- **With the HUD module in the firmware** (a few lines in your keymap, see [Firmware](#firmware)) you get everything live: active layer, pressed keys, modifiers, one-shots, Caps Word, OS, the HUD_TOG key, the other half of a split keyboard.

The HUD protocol (v1) carries the key matrix in 8 bytes, so the module supports matrices up to **8 rows × 8 columns** and up to 32 layers. That covers most split keyboards with up to 7 keys per row and half: Corne, Ferris/Sweep, Kyria, Planck rev6 and the like. Larger matrices (Lily58, Sofle, 60% and bigger boards) stay with the static view for now.

The statistics guess hand and finger from the matrix assuming a split-style matrix (left half in the first rows, thumbs in the last row of each half): on other matrices the per-hand and per-finger figures are approximate.

## Download

From the repository's **Releases** page:

| System | File |
|---|---|
| Windows 10/11 (x86_64) | `keyboard-hud-windows-x86_64.exe` |
| macOS 11+ (Apple Silicon and Intel) | `keyboard-hud-macos-universal.zip` (contains `Keyboard HUD.app`) |
| Linux x86_64 (glibc, Ubuntu 22.04+, recent Fedora) | `keyboard-hud-linux-x86_64` |

Each release also contains `SHA256SUMS.txt` and `keyboard-hud.example.toml`.

**Updates.** Once a day the HUD checks for a new version and tells you in the footer and in the mini indicator (`[update] auto_check = false` to check only by hand). From menu → General → *Check for updates*, or from the tray, you can check whenever you like; if there is a new version, *Install and restart*:
- downloads the file for your system from the release and checks it against `SHA256SUMS.txt` (if it does not match, it is discarded);
- **Windows**: renames the running `.exe` to `<name>.exe.old` (a running executable can be renamed, not overwritten), puts the new one under the same name and restarts; the `.old` file goes away at the next start;
- **Linux**: replaces the file with the new one (same name, executable) and restarts;
- **macOS**: extracts the zip with `ditto` and replaces the whole `Keyboard HUD.app` in its folder, then opens it again.

Configuration, statistics and backups stay where they are. The executable's folder must be writable: otherwise the HUD says so, and you can download by hand from the release page (*What's new…*). Installing only starts from your click. Requests go through the system `curl` (included in Windows 10 and 11, macOS and nearly every Linux distribution) and only to github.com.

## First start

1. Flash a firmware with the HUD module (see [Firmware](#firmware)); on a split keyboard, flash **both** halves. Without the module the HUD still shows the layers, without live state.
2. Put the executable in any folder and start it:
   - **Windows**: double click. If SmartScreen warns you (the executable is not signed), choose *More info → Run anyway*, or unblock the file once and for all (see [Windows: SmartScreen](#windows-smartscreen)).
   - **macOS**: extract the zip. The first time, right click `Keyboard HUD.app` → *Open* (the app is ad-hoc signed, not by a registered Apple developer). Or: `xattr -dr com.apple.quarantine "Keyboard HUD.app"`. The app has no Dock icon: quit it from the HUD's context menu or the tray.
   - **Linux**: `chmod +x keyboard-hud-linux-x86_64 && ./keyboard-hud-linux-x86_64`. A udev rule is needed, once (see [Linux](#linux-udev-rule)).
3. The HUD appears as an overlay. With **heads-up** on (the default) it stays on screen for a moment, then hides on its own and shows up again when you hold a layer key for more than a quarter of a second (quick combinations, which you already know, do not make it flash), when a one-shot is waiting or when you hold a modifier. While it is hidden, a small **marker** stays on its side nearest to the screen edges: the keyboard of the icon, with the keys of layers 1 and 2 in their colors.

**Where the files go.** If the executable's folder is writable, or already contains `keyboard-hud.toml` (portable mode), everything stays next to it. Otherwise the user configuration folder is used (`%APPDATA%\keyboard-hud\config`, `~/Library/Application Support/keyboard-hud`, `~/.config/keyboard-hud`). The files are:

| File | Contents |
|---|---|
| `keyboard-hud.toml` | your configuration (optional) |
| `keyboard-hud-state.toml` | window position in the last session |
| `keyboard-hud-stats.json` | statistics counters |
| `backups/` | automatic backups of the keyboard configuration (`.vil`) |
| `keyboard-hud.log` (+ `.log.1`) | log, at most 1 MB per file |
| `cache/` | Vial definition read from the keyboard |

## Firmware

The firmware side is a small module, [`firmware/hud/`](firmware/hud/), that you add to your own Vial keymap: it answers the HUD's requests and nothing else. It never sends data on its own and never writes to EEPROM, so the keyboard works exactly the same without the HUD. The protocol is described in [docs/PROTOCOL.md](docs/PROTOCOL.md).

In short (details, optional hooks and an example keymap in [firmware/README.md](firmware/README.md)):

1. copy `firmware/hud/` into your keymap folder of [vial-qmk](https://github.com/vial-kb/vial-qmk);
2. in `rules.mk`: `include $(dir $(lastword $(MAKEFILE_LIST)))hud/hud.mk`;
3. in `keymap.c`: `#include "hud.h"` and call `hud_task()` from `housekeeping_task_user()`;
4. optional but recommended: a **`HUD_TOG`** custom keycode, which shows/hides the HUD and toggles click-through from the keyboard.

The module costs under 1 KB of flash: it also fits an ATmega32U4 (Pro Micro) with the stock Vial features.

## Usage

**The menu** (right click on the HUD) has at the top the actions on the clicked key (*Change this key…*, *Undo the last change*) and the most used switches (heads-up, click-through, pause); below, on the left, the tabs **Window** (opacity, scale, view, position), **Style** (theme, styles, keys), **On the keys** (combos, keycast, heatmap), **Tools** (search, training, statistics, export, backup, diagnostics), **General** (language, start with the system). At the bottom *Settings…* and *Quit*. The menu window always has the same size and remembers the last tab.

| Action | How |
|---|---|
| Move the HUD | drag it with the mouse. On Windows it snaps to the edges, the corners and the center of the screen |
| Put it in a corner, the middle of an edge or the center | menu → Window → Position (3×3 grid) |
| Zoom in / out (50–200%) | Ctrl + mouse wheel on the HUD, or menu → Window → Scale |
| Menu (opacity, scale, view, position, theme, heads-up, combos, heatmap, statistics, search, settings, click-through, pause, start with the system, quit) | right click on the HUD |
| Show / hide | one tap of **HUD_TOG**. With heads-up on, the tap *pins* the HUD on screen, and another tap gives it back to heads-up |
| Click-through (clicks go to the windows below) | menu, or **double tap** of HUD_TOG. It turns off only with the double tap |
| Find a key | Ctrl+F or `/` with the HUD focused, or menu → Tools → Search. Arrows to choose, Enter/Esc to close. It also finds combos, tap dances and key overrides |
| See the combos | menu → On the keys → Show combos |
| Start the HUD at login | menu → General → Start with the system (see below) |
| Tray icon | left click: shows/hides the HUD; right click: menu with pause, backup, statistics, settings, start with the system, quit |
| Settings | Ctrl+, with the HUD focused, menu → Settings…, or the tray; Ctrl+Shift+, opens the text file |
| Browse the layers (offline or without the HUD module) | mouse wheel or keys 0–9 |

The menu opens in a small separate window next to the click, so it stays readable even with a small HUD or in the compact view. It closes with ✕, Esc, a click on the HUD or a click elsewhere. On Wayland the compositor decides where it appears.

**Tray icon.** Handy above all with the HUD hidden and on macOS, where the app has no Dock icon. On Linux it goes through D-Bus (StatusNotifierItem), without GTK: KDE, Xfce and most panels show it, GNOME only with the *AppIndicator* extension. Turn it off with `tray = false` in `[appearance]`.

**Start with the system.** Only for your user, without administrator rights: on Windows a value in `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run`, on Linux the file `~/.config/autostart/keyboard-hud.desktop`, on macOS `~/Library/LaunchAgents/io.github.keyboard-hud.plist`. It records the executable's path and the `--x11`, `--vid`, `--pid` and `--verbose` options you started it with. If you move the executable, the box shows unchecked and the tooltip says why: check it again to update the path. In portable mode on a USB stick, it only works with the stick plugged in.

Click-through can only be turned on if the connected keyboard has the HUD_TOG key, so there is always a way to turn it off. If the keyboard is unplugged, it turns off on its own.

**What the HUD shows**

- **Layer**: name and color (from `[[layers]]` in the configuration; by default BASE, LOWER, RAISE, ADJUST). The padlock means a locked layer (`TG`, Layer Lock); a pulsing `1×` means a waiting one-shot. The other active layers appear as chips.
- **Legends**: those of the active layer, as the firmware resolves them (a remap such as Colemak in `keymap_key_to_keycode()` is already applied). Transparent keys show the legend of the layer below, dimmed; layer keys have the color of their target layer; mod-taps show the tap large and the modifier small. The tooltip gives the full name.
- **Modifiers**: Ctrl, Shift, Alt, Gui (Cmd/Opt on macOS). Solid = held, pale = weak, pulsing outline = one-shot, padlock = locked one-shot.
- **Caps Lock / Caps Word**, the **OS** detected or forced, **Num Lock** in the full view, and **QWERTY / Colemak-DH** if the keymap declares it (see [firmware/README.md](firmware/README.md)).
- **Other half disconnected**: if the half without USB stops answering (TRRS cable unplugged), a red warning in the HUD and in the mini indicator. It appears only after the other half has been seen connected, so a non-split keyboard gives no false alarms.
- **Macros**: the key shows what it types or presses (`hi⏎`, `Ctrl+C`, cut to 6 characters) with `M0` small; the tooltip lists every step. Macros are read from the keyboard at every connection, or from the `.vil` offline.
- **Pressed keys**: light up at once and fade in 200 ms.
- **Language**: English or Italian (*Italiano*), from the menu or with `language` in `[appearance]`; with `auto` it follows the system. It changes the interface, tooltips, search, the statistics CSV and new entries of the backups' `CHANGELOG.md`; the log stays in English.
- **Keycast** (menu → On the keys → *Keycast*, or `[keycast] enabled = true`): for videos and presentations, the shortcuts you press appear at the bottom of the screen for two and a half seconds: `Ctrl+Shift+P`, `Alt+Tab`, `F5`, `Esc`, arrows, media keys. **Never the text**: a key that types a character appears only together with Ctrl, Alt or Gui (Shift alone is not enough), and Enter, Space and Backspace never appear alone. Nothing is saved. Mod-taps (Ctrl on the home row) work: the key waits a moment for the modifier to arrive.
- **Change a key without Vial**: right click **on the key** to change → *Change this key…* (first menu entry; opened elsewhere, the menu shows it disabled). Search the new key by name or description (`esc`, `redo`, `MO(2)`, `HUD_TOG`) or write it as in Vial (`C(KC_Z)`, `LCTL_T(KC_A)`, `LT1(KC_SPC)`). Before writing, the HUD warns you if a layer can no longer be reached or if the last `QK_BOOT` or `HUD_TOG` goes away, and asks for confirmation. `QK_BOOT` can only be assigned from Vial. Right before every write a [backup](#automatic-backup) is made and the change goes into the `CHANGELOG.md`; menu → *Undo the last change* puts the previous key back. With a remap such as Colemak on, letters on the remapped layers must be changed in their QWERTY position. Macros, tap dances, combos, key overrides and settings stay in Vial.
- **Export the keymap** (menu → Tools → *Export the keymap…*, or `--export FILE`): an HTML page with a drawing of each layer, in the layer colors and with your labels, plus the list of combos, tap dances, macros and key overrides. It opens in the browser, from which you can print it or save it as PDF. The files go to `export/` next to the configuration.
- **Training** (menu → Tools → *Training…*): the HUD asks for a symbol or a shortcut (`{`, `Ctrl+Z`, `F5`) and waits for you to press it, 20 questions per round. After 6 seconds it shows how (`Lower + D`) and lights the key on the HUD. Keys of the upper layers you use little (from the [statistics](#statistics-and-privacy), if on) and the ones you got wrong come up more often; at the end there are the results and the keys to review. Shift + key also counts for a shifted symbol. Needs the HUD module.
- **Diagnostics** (menu → Tools → *Diagnostics…*): keyboard, firmware protocol, reads per second, answer latency, errors and reconnections, detected OS, other half, file paths, last backup. *Copy all* puts it on the clipboard, ready for a bug report.
- **Mini indicator**: while the HUD is hidden (heads-up or HUD_TOG), a small badge in a corner of the screen reminds you of the states that last after the keys are released: a locked layer (with the padlock), a waiting one-shot, locked modifiers, Caps Lock, Caps Word. It also shows why the keyboard is missing (not found, no permission, paused for Vial): with no keyboard the full HUD stays hidden even without heads-up (tray → *Show the HUD* brings it up), and the tray icon gets an orange dot with the reason in its tooltip and, for two seconds, what just changed: QWERTY/Colemak-DH, OS detected or forced, HUD hidden by HUD_TOG. It does not catch clicks. With `locked_layer = "indicator"` a locked layer no longer keeps up the whole HUD, only the badge; with `oneshot = "indicator"` the same goes for waiting one-shots (layers and modifiers), with `modifiers = "indicator"` for modifiers held for a while.
- **Layer delay**: with heads-up, a held layer (Lower, Raise, tri-layer, `MO`, `LT`) brings up the HUD only after `layer_delay_ms` (default 250 ms). Lower + a key typed in a row, because you already know where it is, do not make the HUD flash; if you hesitate, it shows up. With 0 it shows up at once. Waiting one-shots always show up at once.
- **Full screen**: with a full screen window in the foreground (a game, a video, a presentation) the HUD, the marker and the mini indicator hide and come back when you leave it; the keycast stays, because presentations are where it is needed. Turn it off with `[appearance] hide_in_fullscreen = false` or from Settings. On Windows (a window covering exactly its monitor, not just maximized) and X11 (`_NET_WM_STATE_FULLSCREEN`); on Wayland and macOS it cannot be known.
- **Heads-up marker**: with the HUD hidden by heads-up, a small keyboard like the icon's stays where the HUD will show up, on the side of the HUD nearest to the screen edges (above with the HUD at the top, in the corner with the HUD in a corner, in the center with the HUD in the center; a free position counts as the nearest of the nine in the *Position* menu): it says that the app is running, where the HUD will appear and which keys bring it up (the keys of layers 1 and 2, in their colors). It turns dark on a light background and light on a dark one, reading the pixels around the marker every 2 seconds (see [privacy](#statistics-and-privacy)). On Windows and Linux X11; on Wayland and macOS the system does not allow it without special permissions, and the marker stays light with a dark halo, readable on any background. It does not catch clicks. Turn it off with `[headsup] marker = false` or from the menu (*Marker while hidden*); set its opacity with `marker_opacity` (default 85%) or in Settings. It does not appear with the HUD hidden by HUD_TOG.
- **Tap dances, combos and key overrides** configured in Vial, read from the keyboard at every connection (so also when the HUD resumes after the pause for Vial) or from the `.vil` offline:
  - a tap dance key shows the tap action large and the hold (or double tap) action small; the tooltip lists them all;
  - with *Show combos* each combo is a label with its result between the keys to press together;
  - a key override shows while you use it: holding Shift, the ⌫ key shows ⌦ in color (with the override Shift+⌫ = Del).
  - holding Shift (or with a Shift one-shot), the keys with a symbol show the one they type: `;` becomes `:`, `1` becomes `!`. The layout follows the language of the HUD: Italian keyboard for Italian, US otherwise.
- **Search**: besides keys it finds combos (`combo: J + K together`), tap dance actions (`Q (hold)`) and key overrides (`Shift + Bksp`).

## Linux: udev rule

On Linux, access to `/dev/hidraw*` needs a udev rule. It is the only step with `sudo`, done once. If it is missing, the HUD says so in its status.

```sh
./keyboard-hud-linux-x86_64 --print-udev-rule
```

It prints this rule, the same one Vial recommends (if Vial already works for you, it is probably installed):

```
KERNEL=="hidraw*", SUBSYSTEM=="hidraw", ATTRS{serial}=="*vial:f64c2b3c*", MODE="0660", GROUP="users", TAG+="uaccess", TAG+="udev-acl"
```

```sh
sudo nano /etc/udev/rules.d/59-vial.rules        # paste the line
sudo udevadm control --reload && sudo udevadm trigger
```

Then unplug and plug the keyboard back in.

## Windows: SmartScreen

The executable is not signed with a developer certificate, so the first time Microsoft Defender SmartScreen shows *"Windows protected your PC"*. The warning comes from the "downloaded from the Internet" mark the browser puts on the file, not from a problem with the app. To remove it once and for all:

- right click the `.exe` → *Properties* → check **Unblock** → *OK*;
- or, from PowerShell: `Unblock-File .\keyboard-hud-windows-x86_64.exe`.

Before unblocking it you can compare it with the release's `SHA256SUMS.txt`: `Get-FileHash .\keyboard-hud-windows-x86_64.exe`. If the Defender antivirus flags it (a false positive), you can submit the file to Microsoft for analysis from the *Microsoft Security Intelligence* portal. An executable you build from source has no mark and does not trigger the warning.

## Wayland

On Wayland the compositor decides where windows go and whether they can stay on top: many (GNOME in particular) ignore both requests. If the HUD ends up below other windows or does not remember its position, start it with:

```sh
./keyboard-hud-linux-x86_64 --x11
```

The option forces XWayland, where "always on top" and the position work. Click-through also depends on the compositor.

## Living with Vial

Vial and the HUD use the same Raw HID interface. Answers meant for one of them could end up in the other and confuse it. So:

- if the HUD finds the **Vial** (or VIA) process running, it closes the device and shows *"Paused: Vial is open"*. It checks every 2 seconds;
- when Vial closes, the HUD reconnects on its own and **re-reads the keymap**, which you may have just changed;
- the **Vial web app** (or VIA's) runs in the browser and does not show among the processes. The HUD notices the answers it did not ask for, though: it stops querying the keyboard, shows *"Paused: another program is using the keyboard"* and resumes after 20 seconds of silence, re-reading everything;
- there is also a **manual pause** in the menu.

With the web app, the automatic protection only kicks in after the first exchange between the browser and the keyboard, and starts over if the web app stays idle for more than 20 seconds. **To work safely with the web app, pause the HUD from the menu** and resume when you are done.

The HUD notices keymap changes made by Vial anyway: the firmware module detects them within a few milliseconds and the HUD re-reads the legends. A keymap that remaps keys on its own (such as a Colemak toggle) calls `hud_keymap_changed()` for the same effect.

## Configuration

Copy [`keyboard-hud.example.toml`](keyboard-hud.example.toml) next to the executable as `keyboard-hud.toml`, or to the user configuration folder. Every entry is optional. Changes apply **within a second, without restarting**; only `[device]` needs a restart.

**Settings window** (menu → *Settings…*, tray icon, or **Ctrl+,** with the HUD focused), like VS Code's: every entry of the file grouped by section, with its description, a search box, a dot on the entries that differ from the default and ↺ to reset them. Every change is saved at once in `keyboard-hud.toml`, touching only that entry: comments and order stay. *Open as text* (or **Ctrl+Shift+,**) opens the file in the system editor; if the file contains an error the window shows it and stays read-only until you fix it.

- **Layers**: name and color of each layer, in firmware order, with a color picker (or *neutral*); *Add layer* and ✕ on the last one.
- **Labels**: a card per label with text, description, key (keycode, or matrix row and column), layer and app. An unknown keycode or an incomplete label is flagged on the card. The search at the top also finds labels.
- **Text file**: the whole file with its comments, editable in the window; *Save* (Ctrl+S) only writes a valid file, otherwise it shows the error with its line. It also helps fixing a broken file without leaving the app.

Sliders, numbers and colors are saved when you release the mouse; text fields when you press Enter or move on.

| Section | What it sets |
|---|---|
| `[device]` | `vid` / `pid` to pick the keyboard (default: the first one that answers the HUD protocol) |
| `[appearance]` | tray icon (`tray`), opacity (default 85%), scale, theme (`system`/`dark`/`light`), view (`full`/`compact`), initial position, snap distance (`snap_distance`, 16) and screen margin (`screen_margin`, 8), style (`style`), key opacity and corner radius (`key_opacity`, `key_radius`), combos over the keys (`combos`), language (`language`: `auto`, `en`, `it`), hiding with a full screen window (`hide_in_fullscreen`, default yes) |
| `[headsup]` | on/off, layer delay (`layer_delay_ms`, 250 ms) and modifier delay (400 ms), hide delay (800 ms), opacity when hidden, what a locked layer (`locked_layer`: `hud` or `indicator`), a waiting one-shot (`oneshot`) and a held modifier (`modifiers`) show, always `hud` or `indicator`; marker when hidden (`marker`, default yes) and its opacity (`marker_opacity`, 85%) |
| `[keycast]` | keycast on (default no), position on screen (`position`, default `bottom`) |
| `[indicator]` | mini indicator on, screen corner (`position`, default `top-right`) |
| `[[layers]]` | name and color of each layer, in order |
| `[[labels]]` | custom labels, by keycode (optionally on one layer) or by position, optionally for one application |
| `[stats]` | statistics on, file, period of the "never used" report |
| `[backup]` | automatic backup on, folder, how many to keep (default 30) |
| `[update]` | automatic check for new versions, once a day (`auto_check`, default yes) |
| `[polling]` | read rates (60 Hz visible, 10 Hz hidden) and timeout |
| `[log]` | log level |

Label examples:

```toml
[[labels]]
layer = 2
keycode = "KC_F10"
label = "Step over"
description = "Step over (F10)"

[[labels]]
layer = 2
keycode = "C(KC_DOT)"      # also S(...), A(...), C(S(...)), QK_KB_15 / USER15
label = "Quick fix"

[[labels]]                 # by position: layer, matrix row and column
layer = 0
row = 0
col = 0
label = "Esc"
```

**Per-application labels.** With `app` a label applies only while that program is in the foreground, and for the same key it wins over the generic one. So F10 can say "Step over" in VS Code and stay F10 elsewhere:

```toml
[[labels]]
app = "code"               # also a list: ["idea64", "pycharm64"]
layer = 2
keycode = "KC_F10"
label = "Step over"
```

The name is the process name, without `.exe` and case-insensitive: `code` matches `Code.exe` on Windows and `code` on Linux; on macOS it is the app name. The HUD footer shows which set of labels is active. On Wayland the foreground application cannot be known: only the generic labels apply.

The choices made from the menu (scale, opacity, view, theme, language, style, key opacity and radius, heads-up, combos, keycast) and with Ctrl + mouse wheel are written to `keyboard-hud.toml`, created if missing, touching only those entries: the comments stay. `keyboard-hud-state.toml` only remembers the window position.

## Styles

A style sets the colors and the shape of the HUD: key corner radius and opacity, borders, legend size, window corners and opacity. It does not choose between light and dark: it has colors for both and follows the theme. Choose it from the menu (**Style**) or with `style` in `[appearance]`.

Built in: **default** (slightly rounded keys), **minimal** (no borders, soft keys), **glass** (translucent window and keys; a pressed key turns solid), **high-contrast** (black and white, thick borders, larger legends). Then the colors of well-known themes, dark and light (following *Theme*): **monokai**, **dracula**, **nord**, **solarized**, **gruvbox**, **catppuccin** (Mocha and Latte), **one** (One Dark and One Light), **tokyo-night**. The layer colors stay those of `[[layers]]`.

Your styles are `.toml` files in the `styles/` folder next to the configuration (menu → Style → **Styles folder**); the file name is the style name, and a file with the name of a built-in style replaces it. Every entry is optional:

```toml
# styles/night.toml
key_opacity = 0.8       # 0.2 – 1: the legends stay solid
key_radius = 0.25       # 0 – 0.5 of the key size
border_width = 0        # key border in pixels, 0 = none
legend_scale = 1.1      # 0.7 – 1.5
window_radius = 16      # window corners
window_opacity = 0.7    # applied when you choose the style

[dark]                  # "#RRGGBB" colors; missing ones keep the defaults
background = "#0B0E14"
key = "#1A1F2B"
accent = "#7AA2F7"

[light]
key = "#FFFFFF"
```

Available colors: `background`, `key`, `key_empty`, `key_border`, `pressed`, `pressed_text`, `text`, `text_dim`, `neutral_layer`, `accent`, `chip_off`, `caps_word`, `colemak`, `warning`. The layer colors stay in `[[layers]]`, so that they can match the keyboard's LEDs.

**Save as style** (menu) writes `styles/personal.toml` (or `personal-2.toml`, never over an existing file) with all the current values, colors included: the easiest way to start. Changes to the file of the style in use show within a second; if the file is not valid the error appears and the previous style stays.

Key opacity and corner radius: the `[appearance]` value if set, otherwise the style's. Choosing a style from the menu starts from its values (removes `key_opacity` and `key_radius` and applies the style's window opacity); changing `style` in the file or in Settings leaves the other entries as they are.

## Automatic backup

The HUD saves the keyboard configuration on its own as a `.vil` file, the same format as Vial's export: keymap, encoders, macros, tap dances, combos, key overrides, alt-repeat and QMK settings. No need to remember to export after every change.

- **When**: at connection, when you close Vial (or the web app traffic ends), when the keymap changes and, every 2 minutes with the keyboard idle, for combos and macros, which the firmware does not report. A new file is made **only if something changed** since the last backup of the same keyboard.
- **Where**: the `backups/` folder next to the configuration; open it from menu → Tools → *Backups folder…*. The files are named like `corne_2026-10-22T081530Z.vil` (UTC time); 30 are kept per keyboard.
- **Compare** (menu → Tools → *Backup history…*): pick any two backups and see what changed. The changed keys are highlighted on the drawn keyboard, layer by layer; below is the list in words, with combos, tap dances, macros and settings. *Put it back* next to a key opens *Change key* with the previous value, to check and write. A whole backup loads back in Vial (*File → Load saved layout*).
- **History**: in the same folder, `CHANGELOG.md` tells in words what changed at every backup, newest first. For example:

  ```markdown
  ## 2026-10-22 06:53:40 UTC · Corne · `corne_2026-10-22T065340Z.vil`

  - RAISE · key B (row 2, column 5): Redo → Ctrl+Y
  - Combo added: J + K together → Esc
  - Key override turned off: Shift + Bksp → Del (all layers)
  - tapping term: 180 → 200
  ```

  The same lines go to the log, and the HUD shows a notice with the number of changes.
- **Restore**: in Vial, *File → Load saved layout*. Keycodes are written as numbers, a form Vial accepts: the file is not meant to be read by hand.
- Reads use the same commands as Vial and need no unlocking. Turn it off with `enabled = false` in the `[backup]` section.

## Statistics and privacy

- Only **how many times** each physical key is pressed is counted, per layer and per day (UTC).
- **Never** text, key sequences or timings between keys. The data stays on your computer, in `keyboard-hud-stats.json`, and is never sent anywhere.
- Turn them off with `[stats] enabled = false` and reset them from the statistics window, with confirmation.
- **Heatmap** (menu → On the keys → Heatmap): presses on the shown layer, or on all layers together.
- **Statistics window** (menu → Tools → Statistics): layer usage, split between hands, fingers and rows (how much you work on the home row), most used keys with the combination to reach them, daily activity, keys never used in the last N days, CSV export (`keyboard-hud-stats.csv`).
- **Suggestions**: if a much used key of an upper layer sits in a tiring position (pinky, top and bottom rows, stretch columns), the window suggests swapping it with a comfortable, little used position of the same layer, with the estimated gain. It also tells if one hand works much more than the other. The base layer is never touched: the letter layout is a layout choice. The effort model is simple (finger, row, stretch) and is meant as a hint, not a rule.
- **Updates**: the app's only outgoing connection. A request to `api.github.com` for the latest version (once a day, or never with `[update] auto_check = false` until you ask from the menu) and, only if you install, the download from github.com. Nothing else is sent.
- **Heads-up marker**: to choose whether to draw it light or dark the app reads, every 2 seconds and only with the HUD hidden, the pixels of a small area around the marker (about 50×40). It computes their average brightness and discards them at once: nothing is saved or sent. With `[headsup] marker = false` the screen is never read.
- The count is based on the states read from the keyboard. With the HUD visible it reads at 60 Hz, hidden at least at 30 Hz if the statistics are on. A tap shorter than about 16 ms can be missed.

**A note on security.** Vial keeps the key matrix behind its unlock. The HUD protocol exposes it instead, since that is its whole point. Any local program with access to the HID device can therefore know which physical keys are pressed. On Linux, with the recommended udev rule, access is limited to the logged-in user and the `users` group.

## Offline and simulated modes

- **Offline** (`--offline`, or automatically when there is no keyboard): shows the layers from `vial.json` and a `.vil` file exported from Vial, without live state. Pass the files with `--vial-json` and `--vil`, or put them next to the executable (`vial.json` plus the first `*.vil`). Browse the layers with the mouse wheel or the keys 0–9.
- **Firmware without the HUD module**: the HUD says so, reads the keymap through VIA anyway and shows the layers without live state.
- **Simulated** (`--mock`): a fake Corne loops through a realistic scenario (typing, Lower, Raise, tri-layer, one-shot, Ctrl as mod-tap, Caps Word, a locked layer). It is meant for developing the interface without a keyboard.

## Command line options

```
--mock                simulated keyboard
--offline             no keyboard: vial.json + .vil from disk
--vial-json PATH      Vial definition for the offline mode
--vil PATH            Vial backup (.vil) for the offline mode
--export FILE         export the keymap to FILE (HTML page) and exit; with --offline no keyboard is needed
--vid 0xVVVV          only use devices with this Vendor ID
--pid 0xPPPP          only use devices with this Product ID
--x11                 Linux: force XWayland
--print-udev-rule     print the Linux udev rule and exit
--layer N             layer shown at start in the offline mode
--verbose             detailed log
--version, -h/--help
```

There are also development options, used to capture the states of the interface: `--mock-at`, `--screenshot`, `--screenshot-after`, `--search`, `--heatmap`, `--stats-window`, `--settings`, `--settings-page layers|labels|text`.

On Windows the release executable is a GUI application without a console, so the output of `--help` does not show: the options are the ones listed here.

## Building the app

You need Rust (the version is pinned in `app/rust-toolchain.toml` and `rustup` installs it by itself). No system libraries are needed: on Linux, X11, Wayland and OpenGL are loaded at runtime.

```sh
cd app
cargo run -- --mock          # try it with the simulated keyboard
cargo test                   # unit tests
cargo clippy --all-targets   # must stay clean
cargo build --release        # target/release/keyboard-hud
```

**Release builds.** The workflow [`.github/workflows/release.yml`](.github/workflows/release.yml) builds the three executables on every push and, on every `vX.Y.Z` tag (which must match the version in `app/Cargo.toml`), publishes the release with the files and the checksums:

```sh
git tag v0.1.0 && git push origin v0.1.0
```

The Linux executables are built on Ubuntu 22.04 to stay compatible with older glibc versions. The macOS one is universal (arm64 + x86_64, `lipo`), packaged in an ad-hoc signed `.app`.

**Layout**

```
firmware/hud/          firmware module: the HUD protocol for any Vial keymap
firmware/examples/     example keymap (stock Vial Corne + the module)
firmware/tools/        hud_probe.py: protocol test from the command line
docs/PROTOCOL.md       Raw HID protocol
app/src/hid/           hidapi transport, protocol, worker, simulated keyboard
app/src/vial/          Vial definition, keycodes (generated tables), legends, .vil
app/src/kle.rs         KLE parser (rotations included)
app/src/model.rs       effective keymap, transparent keys, held/locked layers, modifiers
app/src/ui/            egui overlay, header, keyboard, search, statistics
app/src/{config,labels,headsup,search,stats}.rs
app/data/keycodes/     vial-qmk hjson and vial-gui names (source of the tables)
app/data/mock/         the simulated keyboard (definition, .vil, configuration)
```

The keycode tables are **generated** by `build.rs` from vial-qmk's hjson files and vial-gui's `keycodes_v6.py`, copied into `app/data/keycodes/` (the versions are in `SOURCE.md`). To update them, copy the files again and rebuild.

**Protocol test without the app**: `pip install hidapi`, then `python firmware/tools/hud_probe.py info | state --watch | keymap --raw`, with Vial closed.

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| Stuck on "Looking for the keyboard…" | A charge-only cable, or a firmware without Vial. Try `hud_probe.py info`. On Linux check the udev rule |
| "Permission denied on the HID device" | Linux: the udev rule is missing, see above |
| "Firmware without the HUD protocol" | Add the [HUD module](#firmware) to your keymap and flash it (on both halves of a split keyboard) |
| "Paused: Vial is open" | Close Vial: the HUD resumes on its own |
| The HUD does not stay above other windows (Linux) | Wayland: start it with `--x11` |
| I can no longer click the HUD | Click-through is on: double tap HUD_TOG. Restarting the app turns it off anyway |
| The HUD is off screen | At start it comes back on its own to the nearest screen (even if its monitor is unplugged). Otherwise menu → Window → Position |
| The HUD is gone | Heads-up: it shows up again holding a layer key. Or it is hidden: tap HUD_TOG. With a full screen window in the foreground it hides on its own (`hide_in_fullscreen`) |
| The update fails | The executable's folder must be writable, and `curl` is needed. The reason is in menu → General; you can always download by hand from the release |
| Wrong legends after a change in Vial | They update on their own within a second. Otherwise pause and resume from the menu |
| Details | `keyboard-hud.log`, with `--verbose` for the debug level |

## License and credits

- App code: **GPL-2.0-or-later** (license text in [`LICENSE`](LICENSE)). The keycode tables derive from [vial-qmk](https://github.com/vial-kb/vial-qmk) (GPL-2.0-or-later) and [vial-gui](https://github.com/vial-kb/vial-gui) (GPL-2.0).
- Firmware module and example: based on QMK/vial-qmk (GPL-2.0-or-later).
- Font: [DejaVu Sans](https://dejavu-fonts.github.io/) (license in `app/data/fonts/DejaVu-LICENSE.txt`), used for the key symbols.
- Interface: [egui/eframe](https://github.com/emilk/egui); HID access: [hidapi](https://github.com/ruabmbua/hidapi-rs).
