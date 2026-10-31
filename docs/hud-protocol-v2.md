# Keyboard HUD — protocollo v2 (matrici più larghe di 8 colonne)

*Documento per chi mantiene l'app HUD (agente o sviluppatore). Descrive solo le differenze rispetto al protocollo v1 che l'app già conosce.*

## Perché serve

Il protocollo v1 (`docs/PROTOCOL.md` dell'app, modulo QMK `tools/hud/hud.c`) rappresenta i tasti premuti con **un byte per riga** e rifiuta
matrici oltre 8×8. Una tastiera con più di 8 colonne non ci sta. La **Yunzii B68** con il firmware SMK ha una matrice di **5 righe × 16 colonne**
(66 tasti più la pressione della manopola), quindi annuncia **protocollo 2**. Un'app che accetta soltanto `protocollo == 1`, oppure righe/colonne ≤ 8, scarta
la tastiera. È stato verificato: con una build di prova che finge protocollo 1 e 8 colonne, l'app si connette.

Il protocollo 2 **non cambia niente** per le tastiere fino a 8×8: continuano a dichiarare protocollo 1 e a funzionare come prima.

## Cosa deve cambiare nell'app

1. **Accettare `protocol >= 1`** in `GET_INFO` (oggi: valori 1 e 2). Per valori sconosciuti > 2, comportarsi come v2 o rifiutare con un messaggio chiaro; non ignorare silenziosamente.
2. **Non scartare** una tastiera perché `rows > 8` o `cols > 8`. Il limite vero è: `righe × byte_per_riga ≤ 28` (un pacchetto da 32 byte meno 4 di intestazione). La B68 usa 10.
3. **Leggere i tasti premuti da `GET_MATRIX`** (nuovo comando `0x04`) quando `protocol >= 2`; con `protocol == 1` continuare a usare `GET_STATE`.
4. Per il resto (layer, mods, flag, keymap, versione) non cambia niente; `GET_KEYMAP` funziona su qualsiasi dimensione (la posizione è `riga × colonne + colonna`, 16 bit).

## Formato dei pacchetti (32 byte, comune a v1 e v2)

`[0]=0xF0` · `[1]=comando` · `[2]=stato` (0 ok, 1 comando sconosciuto, 2 argomento non valido) · `[3]=numero di sequenza` (rimandato indietro identico) · `[4..]=argomenti o risposta`.
Tutti i campi a più byte sono **little-endian**.

### `GET_INFO` (`0x01`) — risposta

| Byte | Contenuto | v1 | v2 |
| --- | --- | --- | --- |
| 4 | protocollo | `1` | `2` |
| 5 | righe | | |
| 6 | colonne | ≤ 8 | fino a 8 × `byte_per_riga` |
| 7 | numero di layer | | |
| 8–9 | capacità (bit 3 = tasto HUD_TOG presente) | | |
| 10–11 | keycode del tasto HUD_TOG (0 se non c'è) | | |
| 12 | keycode per richiesta di `GET_KEYMAP` (12) | | |
| 13 | versione della keymap (cambia quando la keymap cambia) | | |
| **14** | **byte per riga** = `ceil(colonne / 8)` | non definito (0) | **nuovo** |

Se `protocol == 1`, trattare `byte[14]` come 1.

### `GET_STATE` (`0x02`) — risposta

Tutti i campi come in v1:
`4–7` maschera dei layer attivi (32 bit) · `8` maschera del layer di default · `9` layer bloccati (sempre 0) · `10–17` tasti premuti (vedi sotto) · `18` mods · `19` mods deboli · `20` mods one-shot · `21` mods one-shot bloccati (0) ·
`22` layer one-shot (0xFF = nessuno) · `23` flag (bit0 Caps Lock, bit4 Num Lock; gli altri 0) · `24–26` rilevamento OS (0) · `27` contatore HUD_TOG · `28` versione keymap.

Differenza v2: i byte `10–17` contengono **soltanto le prime 8 colonne** (colonne 0–7) di ciascuna delle prime 8 righe. Un lettore v1 resta quindi corretto per quelle colonne, ma **per vedere tutta
la matrice serve `GET_MATRIX`**.

### `GET_MATRIX` (`0x04`) — nuovo, solo v2

Richiesta: `[F0, 04, 00, seq]`, nessun argomento.
Risposta: dal byte 4 in poi, `righe × byte_per_riga` byte, **riga per riga**; in ogni riga il primo byte contiene le colonne 0–7 (bit 0 = colonna 0), il secondo le colonne 8–15, e così via. Bit a 1 = tasto premuto. I byte oltre quelli usati sono 0.

Il tasto della riga `r`, colonna `c` è premuto se: `reply[4 + r × byte_per_riga + (c >> 3)] & (1 << (c & 7))`.

### `GET_KEYMAP` (`0x03`) — invariato

Richiesta: `[F0, 03, 00, seq, layer, offset_lo, offset_hi, count]` con `count` da 1 a 12 e `offset + count ≤ righe × colonne`; la posizione è `riga × colonne + colonna`.
Risposta: gli argomenti sono ripetuti in `4..7`, poi `count` keycode a 16 bit dal byte 8. I keycode sono quelli effettivi (non solo la base). Stato 2 se gli argomenti sono fuori intervallo.

## Esempio reale (Yunzii B68, 5 righe × 16 colonne, 8 layer)

```
> f0 01 00 05 …                      GET_INFO
< f0 01 00 05 02 05 10 08 08 00 00 7e 0c VV 02 …   VV = versione della keymap

protocollo 2, 5 righe, 16 colonne, 8 layer, capacità 0x0008 (HUD_TOG), keycode 0x7E00, 12 keycode per GET_KEYMAP, 2 byte per riga

> f0 04 00 06                         GET_MATRIX con il tasto Q (riga 1, colonna 1) e il tasto (riga 2, colonna 11) premuti
< f0 04 00 06 00 00 02 00 00 08 00 00 00 00 …
              └riga0┘ └riga1┘ └riga2┘ └riga3┘ └riga4┘
```
Riga 1: `02` = colonna 1. Riga 2: byte alto `08` = colonna 11 (8 + 3).

## Identificare la tastiera

Resta com'è: interfaccia raw HID (usage page `0xFF60`, usage `0x61`), 32 byte, senza report ID. La B68 con SMK ha VID:PID `258a:dead`, numero di serie `vial:f64c2b3c`,
prodotto "SMK Keyboard"; il resto della conversazione (Vial, VIA) funziona come su vial-qmk. Se l'app filtra per nome o per elenco di VID:PID, va aggiornato quel filtro.

## Cose che questo firmware non ha (campi a zero)

Caps Word, Layer Lock, rilevamento del sistema operativo, Colemak, mods one-shot bloccati, tastiera split: i relativi bit/byte restano a 0 e le capacità non sono dichiarate. L'app non deve
aspettarsi che compaiano.

## Verifica

* Firmware: `tests/host/test_hud.c` (compilato due volte: matrice 2×3 → protocollo 1, matrice 5×16 → protocollo 2) e `TestB68Hud` nel simulatore.
* In pratica: `tools/rawhid-test.html` (WebHID) ha i pulsanti **HUD info / state / keymap / matrix** che mostrano le risposte esatte della tastiera collegata.
* Il firmware di diagnosi `yunzii-b68_hudv1_smk.hex` risponde come tastiera v1 a 8 colonne (mostra solo le colonne 0–7), utile per confrontare il comportamento dell'app.
