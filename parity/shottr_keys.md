# Shottr editor keyDown map (from `FUN_1000296c0`, x86_64)

Source: `gh/keyhandler.c` (line numbers = `kh:`) and callees in `gh/kd2.c` (`kd2:`).

## Modifier bits (CONFIRMED)
`FUN_10005a090` (kd2:1908) builds the telemetry key name: bit `0x1` -> "Shift+", bit `0x100` -> "Cmd+".
Option = `0x10000`, Control = `0x1000000` are taken from the task's packing formula. They agree with how the code masks them (`param_3 & 0x10000`, `& 0x1000000` in kh:775, kh:653, kd2 zoom handler), but no string names them, so this is not independently proven.
`FUN_1000bd0d0` reads Option straight from `NSEvent.modifierFlags >> 19` (0x80000).

## Global state used in conditions
| name | meaning | evidence |
|---|---|---|
| `fe0+0x48 == 1` | **no object selected** (`fe0+0x40` = selected object index, Optional<Int>; `fe0+0x50` = objects array of `ProtoObject`) | `FUN_1001d7b00` kd2:3418, `FUN_1001e6a00` kd2:8813 |
| `FUN_1001e6a00()` | **text editing**: the selected object is an `ObjText` whose state is `"editing"` | kd2:8813-9037 |
| `fd8+0x10 == 1` | **an area selection exists** (Shottr calls it the "cutter"; rect at `fd8+0x18..0x30`) | kh:1201-1207, kd2:10008 |
| `c7068+0x60 != 0` | an image is loaded | kh:654, kh:943 |
| object state `"creating"` | an object is still being drawn | kh:256, kd2:5458 |
| `DAT_1002c70a8` = editor view state (`self` in the main handler) | `+0x20` = mode string (`default`/`croptool`/`objectHandlers`/`pan`). `+0x40` = bool array; element 0 must be false for tool hotkeys; elements 0..2 gate the Space hand cursor (probably mouse-button-down flags, INFERRED) | kh:326, kh:957-964, kd2:5395, kd2:3585, kd2:13213 |
| `realPixels` pref | units are pixels (on) or points (off). P toggles it | kh:895, kd2:10083 |

## Dispatch order inside `FUN_1000296c0`
1. Record the key in a held-keys set that expires after 0.8 s (`FUN_1000e3f10`, timer `DAT_100243f88`=0.8) (kh:87).
2. **Text editing** -> every key goes to the object handler `FUN_1001e10f0`, then return (kh:93-103).
3. Tab / Esc / Cmd+C / Cmd+Q / F2 special cases (kh:117-527).
4. Cmd+Z / Cmd+Shift+Z (kh:528-649).
5. Copy colour: Tab / Shift+Tab / I (kh:654-856).
6. P / Cmd+P / Cmd+A (kh:858-942).
7. Backdrop handler `FUN_1001ba800` (kh:943-956).
8. Tool hotkeys (no selection, no Shift/Cmd), V, K (kh:957-1137).
9. Area-selection shortcuts: B, Delete, R, O, S, M (kh:1138-1388).
10. Ruler `FUN_1000bd0d0`, debug combos, Cmd+O OCR (kh:1389-1808).
11. Area-selection handler `FUN_100183190`, which runs when a selection exists (kh:1811).
12. Object handler `FUN_1001e10f0`, which runs when an object is selected, or for Cmd+V when none is (kh:1816-1827).
13. Zoom handler `FUN_10001fae0` for every key. Then `FUN_10002e450` updates the cursor (open-hand / closed-hand / zoom / default from held keys and mode), and `FUN_10014b050` refreshes the image/selection size readout (px/pt) (kh:1833-1838).

## Key table

| key+mods | action | condition | evidence | status |
|---|---|---|---|---|
| **A** | Arrow tool (`ObjArrow`) | no area selection, no Shift/Cmd (Opt/Ctrl allowed), image loaded, `self+0x40[0]+0x20==0` (flag not identified) | kh:936,994-997 -> `FUN_10014de00` (kh:1035) | CONFIRMED |
| **R** | Rectangle tool (`ObjRectangle`) | same as A | kh:970-977 | CONFIRMED |
| **O** | Oval tool (`ObjOval`) | same as A | kh:989-991 | CONFIRMED |
| **L** | Line tool (`ObjLine`) | same as A | kh:985-987 | CONFIRMED |
| **T** | Text tool (`ObjText`) | same as A | kh:978-979 | CONFIRMED |
| **B** | Blur/conceal tool (`ObjConcealed`; `FUN_10014de00` maps "conceal"->"blur") | same as A | kh:999-1002, kd2:3532 (`conceal`/`blur`) | CONFIRMED |
| **C** | Counter tool (`ObjCounter`) | same as A (`local_78` = key==C, kh:451/166) | kh:1004-1007 | CONFIRMED |
| **S** | Spotlight tool (`ObjSpotlight`) | same as A | kh:1009-1011 | CONFIRMED |
| **D** | Freehand tool (`ObjFreehand`) | same as A | kh:1041-1044 | CONFIRMED |
| **H** | Highlighter tool (`ObjHighlighter`) | same as A | kh:1046-1049 | CONFIRMED |
| **V** | Default (select/move) tool: deselect all objects (`FUN_1001d7b00`), set mode to "default", telemetry `DefaultTool` | no area selection, no Shift/Cmd | kh:1054-1088 | CONFIRMED |
| **K** (keycode 0x28 **or** typed char "k") | Deselect, set mode to default, then **Backdrop**. If the image already has a backdrop, shows "Window already has backdrop" | no area selection, no Shift/Cmd | kh:1091-1133 -> `FUN_100150290` (kd2:13496; strings `backdropApplied`, 0x1002772b0) | CONFIRMED (that it opens the backdrop UI is INFERRED) |
| **M** | Turns the area selection into a **Magnifier** object, then clears the selection | area selection exists, no Shift/Cmd/Opt | kh:1306-1312 -> `FUN_1001684c0` (kd2:9263, `addObject` + "imprint:") | CONFIRMED |
| **R / O / S** (with area selection) | Turns the selection into a Rectangle / Oval / Spotlight object (`imprint`), then clears the selection | area selection exists, no Shift/Cmd/Opt | kh:1147-1152, 1294-1305, `FUN_1001684c0` | CONFIRMED |
| **B** (with area selection, any mods) | Turns the selection into an `ObjConcealed` (blur) object (`FUN_1001362f0`), clears the selection, telemetry `draw`, returns | area selection exists | kh:1142,1199-1291 | CONFIRMED |
| **Delete / Fwd-Delete** (with area selection, any mods) | **Erase the selected area**: `FUN_1001688b0` makes an `ObjConcealed` in "erase" mode, telemetry `erase` | area selection exists | kh:1143,1315-1383, kd2:5187 | CONFIRMED |
| **Delete / Fwd-Delete** (object selected) | Delete the selected object (`FUN_1001e6290`), telemetry `delete`, undo checkpoint | object selected, not text-editing | kd2:5957-6128 | CONFIRMED |
| **Tab** | Copy the colour of the pixel under the cursor (format from pref `colorFormat`, default "HEX"), show "Copied to the clipboard" toast, telemetry `copyColor` | image loaded, no object selected, no helper popover / backdrop panel visible (`FUN_1000991c0`) | kh:117-160, 654-741 | CONFIRMED |
| **Shift+Tab** | Copy the **darkest** colour in a 20x20 area centred on the cursor (cursor-10, size 20). Toast "Darkest color: ..." | same as Tab | kh:742-853 (`"darkest"`, `DAT_1002442f8`=-10, `DAT_100243f78`=20) | CONFIRMED |
| **I** / **Shift+I** | Same as Tab / Shift+Tab (copy pixel colour / darkest). No Cmd check, and no "no object" check | image loaded | kh:654-655 (`param_2 == 0x22`) | CONFIRMED |
| **Tab** (object selected) | Select the **next object** (index+1, wraps to 0) | object selected, not text-editing | kd2:5931-5953 (`FUN_1001e6030`) | CONFIRMED |
| **Tab** (while editing text) | Calls `FUN_10006ff90` with the current colour string (`AppLogic+0x20`), probably inserting the colour code into the text | text-editing an `ObjText` | kd2:5810-5852 | INFERRED |
| **C** (with area selection) | Copy the **average colour** of the selection, telemetry `copyColorAvg`, toast | area selection, no Cmd | kd2:10164-10278 (`"average"`) | CONFIRMED |
| **Cmd+C** (nothing selected) | `copyImage(selection:false, dontHide:false)`: copy the whole image, then **hide the editor** | no area selection, no object selected | kh:450-522, `FUN_100156fe0` kd2:4919 (string `copyImage(selection:dontHide:)`) | CONFIRMED |
| **Cmd+C** (with area selection) | `copyImage(selection:true)`: copy only the selected area, then clear the selection | area selection | kd2:10280-10287 | CONFIRMED |
| **Cmd+C** (object selected) | Copy the object (`copyObject`). Puts "Shottr <Type>" text on the general pasteboard, with an image path for image objects | object selected, not text-editing | kd2:6190-6660 | CONFIRMED (pasteboard details partly INFERRED) |
| **Cmd+V** | Paste a copied Shottr object (JSON decode of the internal clipboard `fe0+0x98/0xa0`). The paste counter `+0xa8` is multiplied by 10, which looks like a 10-px offset per paste. Telemetry `draw`/`toolbar` | not text-editing. Skipped if `FUN_1001d9ad0()` is true or the internal clipboard is empty | kh:1818-1820, kd2:5493-5620 | CONFIRMED (offset INFERRED) |
| **Cmd+D** | Duplicate the selected object (`FUN_1001e6ee0(10)`, probably a 10-px offset), telemetry `duplicate` | object selected | kd2:6133-6188 | CONFIRMED (offset meaning INFERRED) |
| **Cmd+]** / **Cmd+[** | Selected object's size **+1 / -1** (vtable+0x2f8(±1)), telemetry `edit` / editType `size` | object selected | kd2:6672-6880, 6885-7002 | CONFIRMED |
| **Cmd+Return / Cmd+Enter(keypad)** | If the selected object is a **Ruler**, convert it to an **Arrow** with `arrowType` changed 0->2 (probably double-headed), telemetry `convertObject from:ruler to:arrow` | object selected, not editing, nothing in "creating" state | kd2:7040-7215 | CONFIRMED (arrowType meaning INFERRED) |
| **Cmd+R** (object selected) | Only records an undo checkpoint here. The real effect, if any, is in the object's own key handler | object selected, not editing | kd2:6662-6670 | INFERRED |
| any other key (object selected) | Passed to the selected object's own `keyDown` (vtable+0x2c0). Object nudging with arrows probably happens there | object selected | kd2:5705-5710 | INFERRED |
| **← → ↑ ↓** (area selection) | **Nudge the selection**. Step = 1 unit, **Shift = 10 units**. Unit = 1 px when `realPixels` is on, otherwise the scale factor (1 pt). Telemetry `cutterMoveResize` | area selection, no Cmd | kd2:10085-10130, 10289-10300, 10508-10600; `DAT_100242b48`=1.0, `DAT_100243f68`=10.0 | CONFIRMED |
| **Cmd+← / Cmd+→** (area selection) | Resize the selection: the **right edge** moves left/right (w ∓/± step, origin.x unchanged), logged as "x2" | area selection | kd2:10370-10407 | CONFIRMED |
| **Cmd+↓ / Cmd+↑** (area selection) | Resize the selection: the **bottom edge** moves down/up. Cocoa y points up: Cmd+↓ gives y -= step, h += step. Cmd+↑ gives y += step, h -= step. Logged as "y". Together with Cmd+←/→ this drags the bottom-right corner | area selection | kd2:10408-10466 | CONFIRMED |
| **] or 9** (area selection) | **Grow the selection** by step on all 4 sides (w,h += 2·step). Shift = 10 | area selection, any Cmd | kd2:10727-10800 | CONFIRMED |
| **[ or 8** (area selection) | **Shrink the selection** by step on all sides. Shift = 10 | area selection | kd2:10840-10870 | CONFIRMED |
| **A** (area selection) | **Auto-adjust** the selection (`FUN_10017df30`, telemetry `adjust`), presumably snapping to content | area selection | kd2:10131-10162 | CONFIRMED (snap behaviour INFERRED) |
| **Return / Enter** (area selection) | **Crop** to the selection (`FUN_10014c860(0)`, telemetry `crop`) | area selection | kd2:10954-10990 | CONFIRMED |
| **Esc** (area selection) | **Clear the selection** (`FUN_10017f3a0`, telemetry `resetCutter`) | area selection | kd2:10992-11029 | CONFIRMED |
| **Cmd+O** (area selection) | **OCR the selection** (`FUN_1001606f0`, telemetry `ocr_api`) | area selection | kd2:11031-11090 | CONFIRMED |
| **Cmd+O** (no selection) | OCR the whole image if width and height are each ≤1000 (image px / scale). Otherwise shows "Select area first / Select rectangle with text that you want to recognize and copy" | no area selection, image loaded | kh:1647-1808 (strings 0x10026f7e0/0x10026f800) | CONFIRMED |
| **P** (no Cmd) | Toggle **px/pt units** (`realPixels` pref), telemetry `setting-realPixels` | any state (outside text editing) | kh:858-898 | CONFIRMED |
| **Cmd+P** | **Pin** the screenshot (`FUN_100165320` = `pinImage(midX:midY:)`) | - | kh:899-929 | CONFIRMED |
| **Cmd+A** | **Add capture**: hide the editor and grab another area to append (`FUN_10014f860`). Licence-gated (string "Adding Capture is only available after the license activation" is referenced). Beeps "No Screenshot" if there is no image | - | kh:936-941, kd2:3077 | CONFIRMED |
| **Cmd+Z** | Remove Z/Y from the held-key set, redraw, flush pending state. **No `undo` call is made here**, so undo presumably comes from the Edit-menu NSUndoManager. Also matches typed char z/Z for non-QWERTY layouts | not text-editing (editing routes Cmd+Z to the text object) | kh:528-577, 629-648 | CONFIRMED (undo itself INFERRED) |
| **Cmd+Shift+Z** | **Redo** (`[undoManager redo]`) | not text-editing | kh:578-625 | CONFIRMED |
| **Esc** | Checks in this order, stopping at the first that applies: (1) text editing -> sent to the object. (2) A non-empty pending string `fe0+0x68` (cleared by deselect `FUN_1001d7b00`; its writer was not found) or an object in "creating" state -> finish it, set tool to "default" (kd2:5331-5410). (3) A visible helper popover (zoom/colour/pixels/help) or backdrop panel -> close it (and the NSColorPanel), then return (`FUN_100099290`). (4) Mode (`self+0x20`) is `croptool` or `objectHandlers` -> reset to "default". (5) No area selection and no object selected -> **hide the editor**: if `copyOnEsc` (default **on**) copy the image first, if `saveOnEsc` (default off) save it (`saveImage`), if neither just hide. (6) Area selection -> clear it. Object selected -> sent to the object's own handler | see left | kh:204-449, kd2:5331-5470, kd2:3938, kd2:10992 | CONFIRMED (step 6 object behaviour INFERRED) |
| **Cmd+Q** | `cmdQAction` pref: `quit` -> terminate, `close` -> hide editor, unset ("?") -> ask with a Quit/Close dialog | - | kh:167-193, `FUN_10014bae0` kd2:9377 | CONFIRMED |
| **F2** | **Upload** the image (`FUN_10015bcc0`: `uploadMode` none/flop/s3, S3 creds, "Upload Failed"). Licence gating INFERRED ("Activate" string referenced) | - | kh:194-202, kd2:11858 | CONFIRMED |
| **← → ↑ ↓, 1, 2** (nothing selected) | **Ruler** (`DAT_1002c6fd0`, `FUN_1000bb1d0`). Keys pressed within 0.8 s form two mutually exclusive groups, {←,→,2} and {↓,↑,1}. Shift and Option are passed through. Any area or object selection turns the ruler off | no area selection, no object selected | kh:1405, kd2:2000-2195 (key table `DAT_1002b8240` = 7b,7c,7d,7e,12,13) | CONFIRMED wiring, INFERRED that it is a ruler (names: `ObjRuler`, `ruler:sensitivity`) |
| **[ / ]** (nothing selected) | **Backdrop shadow** -0.1 / +0.1, clamped to 0..1, telemetry `backdrop:shadow` | backdrop active (`fe8+0x168==1`; meaning INFERRED), no selection/object, image | kh:943-956, kd2:9039-9141 (`DAT_100247440` = +0.1, -0.1) | CONFIRMED |
| **'** (quote, keycode 0x27 or typed char "'") | Backdrop action `FUN_1001b14d0` (not decompiled) | no selection/object, image | kd2:9058-9097 | INFERRED |
| **Shift+=** / **Shift+keypad +** | Zoom in (`FUN_10001eee0(1)`) | Shift required, even for keypad keys (every zoom case returns when bit 0 is clear) | kd2:2339-2373 | CONFIRMED |
| **Shift+-** / **Shift+keypad -** | Zoom out (`FUN_10001eee0(0)`) | Shift | kd2:2378-2410 | CONFIRMED |
| **Shift+0** / **Shift+keypad 0** | Zoom reset/fit (`FUN_10001f0f0`) | Shift | kd2:2411-2445 | CONFIRMED key, INFERRED meaning |
| **Shift+1** / **Shift+keypad 1** | Zoom preset `FUN_10001f530(0)` (probably 100% / 1:1) | Shift | kd2:2311-2338 | CONFIRMED key, INFERRED meaning |
| **Shift+2** / **Shift+keypad 2** | Zoom preset `FUN_10001f750()` (probably 200% or fit) | Shift | kd2:2339-2345, 2446-2475 | CONFIRMED key, INFERRED meaning |
| **Space** (held) | No branch in the main, selection, object or zoom handlers. While Space is in the held-key set, the per-key cursor update `FUN_10002e450` shows an **open-hand** cursor (pan) unless `self+0x40[0..2]` is set. In `pan` mode, dragging shows a **closed-hand** cursor. Space is also forwarded to a selected object's keyDown | any | kd2:13193-13240 (`FUN_1000e3c00(0x31)`, "open-hand"), kd2:13280-13300 ("closed-hand") | CONFIRMED (that the pan itself happens on mouse drag is INFERRED) |
| **Z / Y** (held, no Cmd) | Shows the **zoom** cursor while Z (0x06) or Y (0x10) is in the held-key set (`DAT_1002b1698`). Cmd+Z removes Z/Y from the set so the cursor does not stick | any | kd2:13240-13262, 13420-13430; kh:542-549 | CONFIRMED (click-to-zoom behaviour INFERRED) |
| **Cmd+Opt+Ctrl+X** | Debug: notification with OS + app version ("Shottr (CFBundleVersion)") | - | kh:1408-1494 | CONFIRMED |
| **Cmd+Opt+Ctrl+T** | Debug: toggle Telemetry `localDumping`, notification "Dumping true/false" | `FUN_10005a3b0()` gate | kh:1495-1601 | CONFIRMED |
| **Cmd+Shift+Ctrl+L** | **Reset all settings** (prints the message, `removePersistentDomainForName`) | - | kh:1602-1621, kd2:9205 | CONFIRMED |
| **Cmd+Shift+K** | Debug: `FUN_100057740` + print `AppLogic.metadata` | - | kh:1622-1645 | CONFIRMED |

## Keys with no handler found
These keys have no branch in `FUN_1000296c0`, `FUN_100183190`, `FUN_1001e10f0`, `FUN_10001fae0` or `FUN_1001ba800`: **digits 3–7** (and 8/9 without a selection), **E, F, G, J, N, U, W, X** (except the debug combo), **Y** (only the held cursor), **M without a selection**, `;` `,` `.` `/` `\` `` ` ``, **Return without a selection or object**. When an object is selected they are still passed to its own keyDown (vtable+0x2c0, not analysed).

## Constants
- Selection nudge/resize step: 1.0 (`DAT_100242b48`), with Shift 10.0 (`DAT_100243f68`). In pixels when realPixels is on. Otherwise multiplied by the scale returned by `FUN_100014550` (probably the backing scale factor, i.e. 1 pt / 10 pt).
- Shift+Tab "darkest" search box: 20x20 px centred on the cursor (offset -10).
- Backdrop shadow step: ±0.1, clamped to [0, 1].
- Object size step (Cmd+[ ]): ±1 (meaning of the unit depends on the object).
- Duplicate: `FUN_1001e6ee0(10)` (10, probably a 10-px offset). Paste: counter×10.
- OCR without a selection: allowed only if image width/scale and height/scale are both ≤1000.
- Held-key memory for the ruler: 0.8 s timer.
- Prefs: `copyOnEsc` default true, `saveOnEsc` default false, `colorFormat` default "HEX", `cmdQAction` default "?" (ask), `defaultColor` "#FF0C01".

## Not determined
- What the zoom functions do exactly (`FUN_10001f530`, `FUN_10001f750`, `FUN_10001f0f0`, `FUN_10001eee0`). They were not decompiled.
- How individual objects handle keys (vtable+0x2c0), e.g. whether arrows nudge a selected object and by how much, and what Cmd+R does to an object.
- The `'` backdrop action (`FUN_1001b14d0`) and the ruler internals (`FUN_1000bb1d0`: how Shift and Option change it, and which group is horizontal).
- The gating flag `self+0x40[0]+0x20` that disables tool hotkeys (perhaps a text field or popover having focus).
- What `fe0+0x68` holds (non-empty makes Esc commit instead of closing the editor). It is cleared by `FUN_1001d7b00`, but the code that sets it was not found.
- Option/Control bit positions are not independently confirmed by any string.
- `FUN_1001d9ad0` (the condition that skips Cmd+V paste), and what `FUN_10006ff90` does with Tab during text editing.
- Whether Cmd+Z undo really goes through the Edit menu; this handler never calls `undo`.
