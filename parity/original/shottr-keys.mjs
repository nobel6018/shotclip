// Model of Shottr 1.10.0b's editor key dispatch, transcribed from the x86_64 decompile
// (MainWindowController keyDown -> FUN_1000296c0) and the MainMenu.nib key equivalents.
// Shottr cannot run on Windows, so this model is the "original" side; rea-check --record
// writes its answers to golden/keys.json. macOS Cmd is mapped to Windows Ctrl.
//
// stdin:  { key, shift?, ctrl?, alt?, ctx }   ctx = none | area | object | text | drawing
// stdout: { action }
//
// Only behaviour read directly from the decompile/nib is modelled; anything else is "unknown"
// and must not appear in the parity inputs.

const input = JSON.parse(await new Promise((r) => {
  let s = '';
  process.stdin.on('data', (d) => (s += d)).on('end', () => r(s));
}));

function act({ key, shift = false, ctrl = false, alt = false, ctx }) {
  const k = key.length === 1 ? key.toUpperCase() : key;
  const plain = !shift && !ctrl; // tool hotkeys test (mods & 0x101) == 0 (Shift, Cmd)
  const area = ctx === 'area';

  // kh:93-103 text editing: every key goes to the ObjText.
  if (ctx === 'text') return 'text:input';

  // kh:117-527 Esc: drawing -> finish the object; area -> clear (kd2:10992); none -> copy & hide.
  if (k === 'Esc' && !ctrl) {
    if (ctx === 'drawing') return 'draw:finish';
    if (area) return 'area:clear';
    if (ctx === 'none') return 'finish';
  }
  // Cmd+C: kh:450-522 (none: copyImage + hide), kd2:10280 (area), kd2:6190 (object).
  if (k === 'C' && ctrl && !shift) {
    if (area) return 'area:copy';
    if (ctx === 'object') return 'obj:copy';
    return 'copy:image';
  }
  // kh:528-649, menu: Cmd+Z undo, Cmd+Shift+Z redo ('Z' key equivalent).
  if (k === 'Z' && ctrl) return shift ? 'redo' : 'undo';
  // Tab: object selected -> next object (kd2:5931); else colour under cursor (kh:654-853).
  if (k === 'Tab' && !ctrl) {
    if (ctx === 'object') return 'obj:next';
    return shift ? 'color:darkest' : 'color:pixel';
  }
  // kh:655 I / Shift+I: same as Tab / Shift+Tab, no object check.
  if (k === 'I' && !ctrl) return shift ? 'color:darkest' : 'color:pixel';
  // kh:858-929 P = px/pt units, Cmd+P = pin; kh:936 Cmd+A = add capture.
  if (k === 'P' && !shift) return ctrl ? 'pin' : 'units';
  if (k === 'A' && ctrl && !shift) return 'addcapture';
  // F2 upload (kd2 FUN_10015bcc0); menu Cmd+e upload, Cmd+s save, Cmd+S save as.
  if (k === 'F2') return 'upload';
  if (k === 'E' && ctrl && !shift) return 'upload';
  if (k === 'S' && ctrl) return shift ? 'saveas' : 'save';
  // menu 'R' with Cmd = Cmd+Shift+R rasterize; 'V' with Cmd = Cmd+Shift+V open from clipboard.
  if (k === 'R' && ctrl && shift) return 'rasterize';

  if (!area) {
    // kh:968-1052 tool hotkeys.
    if (plain) {
      const tools = { A: 'arrow', R: 'rect', O: 'oval', L: 'line', T: 'text', B: 'blur', C: 'counter', S: 'spotlight', D: 'pen', H: 'highlight' };
      if (tools[k]) return 'tool:' + tools[k];
      if (k === 'V') return 'tool:select'; // kh:1054
      if (k === 'K') return 'backdrop'; // kh:1091
      if (k === 'M') return 'tool:magnifier'; // MainMenu.nib "Magnifier" key 'm'
      if (k === 'Q') return 'corner:topleft'; // MainMenu.nib "Selection Top Left" 'q'
      if (k === 'W') return 'corner:bottomright'; // MainMenu.nib "Selection Bottom Right" 'w'
    }
  } else {
    // kh:1138-1388: area selection + no Shift/Cmd/Opt (B and Delete with any mods).
    const bare = !shift && !ctrl && !alt;
    if (k === 'B') return 'area:blur';
    if (k === 'Delete' || k === 'Backspace') return 'area:erase';
    if (bare) {
      const conv = { R: 'rect', O: 'oval', S: 'spotlight', M: 'magnifier' };
      if (conv[k]) return 'area:' + conv[k];
    }
    // FUN_100183190 (kd2:10085-11090) area handler.
    const step = shift ? 10 : 1;
    const dir = { Left: [-1, 0], Right: [1, 0], Up: [0, -1], Down: [0, 1] }[k];
    if (dir) {
      const [dx, dy] = dir.map((v) => v * step);
      return ctrl ? `area:resize:${dx},${dy}` : `area:move:${dx},${dy}`;
    }
    if (k === ']' || k === '9') return `area:grow:${step}`;
    if (k === '[' || k === '8') return `area:grow:${-step}`;
    if (k === 'C' && !ctrl) return 'area:avgcolor';
    if (k === 'A' && !ctrl) return 'area:autoadjust';
    if (k === 'Enter') return 'area:crop';
    if (k === 'O' && ctrl) return 'area:ocr';
  }
  if (ctx === 'object') {
    // kd2:5957 Delete, kd2:6133 Cmd+D, kd2:6672-7002 Cmd+] / Cmd+[.
    if (k === 'Delete' || k === 'Backspace') return 'obj:delete';
    if (k === 'D' && ctrl) return 'obj:duplicate';
    if (k === ']' && ctrl) return 'obj:size:+1';
    if (k === '[' && ctrl) return 'obj:size:-1';
  }
  if (k === 'V' && ctrl && !shift) return 'obj:paste'; // kh:1818
  // kh:1647-1808 Cmd+O with nothing selected: OCR the whole image.
  if (k === 'O' && ctrl && ctx === 'none') return 'ocr';
  // FUN_1000bd0d0 ruler: arrows / 1 / 2 with nothing selected.
  if (ctx === 'none' && !ctrl && ['Left', 'Right', 'Up', 'Down', '1', '2'].includes(k)) return 'ruler';
  // menu: Cmd++ / Cmd+- / Cmd+1 fit / Cmd+0 100% / Cmd+2 selection; FUN_10001fae0 Shift+= / Shift+-.
  if (ctrl && !shift) {
    const z = { '=': 'zoom:in', '-': 'zoom:out', '1': 'zoom:fit', '0': 'zoom:100', '2': 'zoom:selection' };
    if (z[k]) return z[k];
  }
  if (shift && !ctrl && k === '=') return 'zoom:in';
  if (shift && !ctrl && k === '-') return 'zoom:out';
  return 'unknown';
}

process.stdout.write(JSON.stringify({ action: act(input) }));
