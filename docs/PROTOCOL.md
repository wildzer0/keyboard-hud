# HUD protocol v1 (Raw HID)

> Matrices wider than 8 columns use **protocol 2** (`GET_INFO` byte 14 = bytes per row, new command `0x04 GET_MATRIX`): see [hud-protocol-v2.md](hud-protocol-v2.md). The app speaks both.

Transport: the VIA/Vial Raw HID interface (usage page `0xFF60`, usage `0x61`), 32-byte reports.
On every platform `hidapi` writes 33 bytes, with report ID `0x00` in front.
Request/response model: the firmware never sends reports on its own.
All multi-byte fields are **little-endian**; reserved bytes are 0.

Firmware side: `raw_hid_receive_kb()` in [`firmware/hud/hud.c`](../firmware/hud/hud.c). VIA calls this function
for the commands it does not know, then sends the same buffer back.

## Common header

| Byte | Request | Response |
|---|---|---|
| 0 | `0xF0` | `0xF0`, or `0xFF` (`id_unhandled`) if the firmware does not support the HUD |
| 1 | subcommand | subcommand (echo) |
| 2 | `0xFF` | status: `0` OK, `1` unknown subcommand, `2` invalid argument |
| 3 | `seq` chosen by the host | `seq` (echo), to drop stale responses |
| 4–31 | arguments | payload |

If the status in the response is still `0xFF`, the buffer came back unchanged. That happens while Vial is unlocking
(`vial_unlock_in_progress`): VIA then drops every non-Vial command. The host should retry later.

## `0x01 GET_INFO`

| Byte | Field |
|---|---|
| 4 | HUD protocol version (1) |
| 5 | matrix rows (`MATRIX_ROWS`, 1–8) |
| 6 | matrix columns (`MATRIX_COLS`, 1–8) |
| 7 | layer count (1–32) |
| 8–9 | capabilities: b0 Colemak, b1 Caps Word, b2 OS detection, b3 HUD_TOG, b4 Layer Lock, b5 split |
| 10–11 | keycode of `HUD_TOG` (0 if there is none) |
| 12 | maximum keycodes per `GET_KEYMAP` (12) |
| 13 | `keymap_version` |

## `0x02 GET_STATE`

| Byte | Field |
|---|---|
| 4–7 | `layer_state` (u32) |
| 8 | `default_layer_state` (u8, one bit per layer) |
| 9 | layers locked with `QK_LLCK` (bitmask, layers 0–7) |
| 10–17 | matrix: byte *r* = row *r*, bit *c* = column *c*. On a split keyboard the master reports both halves |
| 18 | `get_mods()` |
| 19 | `get_weak_mods()` |
| 20 | `get_oneshot_mods()` |
| 21 | `get_oneshot_locked_mods()` |
| 22 | active one-shot layer (`0xFF` = none) |
| 23 | flags: b0 Caps Lock, b1 Caps Word, b2 Colemak, b3 user flag (`hud_user_flag()`), b4 Num Lock, b5 other half connected |
| 24 | detected OS (`os_variant_t`): 0 unsure, 1 Linux, 2 Windows, 3 macOS, 4 iOS |
| 25 | OS mode: 0 auto, 1 Windows, 2 macOS, 3 Linux |
| 26 | effective OS: 1 Windows, 2 macOS, 3 Linux, 0 unknown |
| 27 | `hud_toggle_count`: +1 on every `HUD_TOG` press, wraps after 255 |
| 28 | `keymap_version` |

Modifiers (bytes 18–21), QMK 8-bit format: b0 LCtl, b1 LSft, b2 LAlt, b3 LGui, b4 RCtl, b5 RSft, b6 RAlt, b7 RGui.

## `0x03 GET_KEYMAP`

Request: byte 4 = layer, 5–6 = offset (u16), 7 = count (1–12).
Response: bytes 4–7 = echo of the request, 8–31 = `count` keycodes (u16).

`offset` is the position `row × columns + column`. Keycodes come from `keymap_key_to_keycode()`,
so they are **effective**: a remap done there (such as a Colemak toggle) is already applied.
If layer, offset or count are out of range, the status is `2`.

## `keymap_version`

Increases by 1 when the effective keymap changes:
- when the keymap calls `hud_keymap_changed()` (e.g. after a Colemak toggle);
- when Vial changes a key. Vial does not notify the keymap, so the master re-reads the raw keymap
  8 positions per cycle and computes a 32-bit FNV-1a hash of it. If the hash changed at the end of a pass,
  the version increases. A full pass takes a few milliseconds.

When the version changes, the HUD re-reads the keymap with `GET_KEYMAP`.

## VIA/Vial commands used by the HUD

| Command | Use | Notes |
|---|---|---|
| `0xFE 0x00` | Vial protocol (u32 LE) and UID (8 bytes) | key of the definition cache |
| `0xFE 0x01` | definition size (u32 LE) | |
| `0xFE 0x02` + page (u16 LE) | 32 bytes of definition | xz-compressed JSON |
| `0xFE 0x0D 0x00` | number of tap dances, combos, key overrides (one byte each) | `vial_dynamic_entry_op`; a firmware without this command returns the request unchanged |
| `0xFE 0x0D` + `0x01`/`0x03`/`0x05` + index | status (0 = OK) and 10 bytes: tap dance, combo or key override | structures from `quantum/vial.h`, LE. Read at every connection, without unlocking |
| `0x02 0x02` | layout options (u32 **BE**) | to filter the KLE alternatives |
| `0x11` | layer count | |
| `0x12` | raw keymap (offset u16 **BE**, size ≤ 28, keycodes **BE**) | without remaps; only for comparison or in the degraded mode |
