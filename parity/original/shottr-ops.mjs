// Model of Shottr 1.10.0b area-selection key effects and Shift+Tab "darkest", transcribed from
// the x86_64 decompile (FUN_100183190 kd2:10085-11090 area handler; kh:742-853 darkest).
// Only what the decompile states is modelled: step 1 (Shift 10), plain arrows move the whole
// selection, Cmd+arrows move the right/bottom edge, ] / 9 grow and [ / 8 shrink every side by
// the step. Clamping at the image border is NOT established, so inputs stay inside the image.
//
// stdin:  {op:"area", rect:[x0,y0,x1,y1], w, h, key, shift?, ctrl?}  -> {rect}
//         {op:"darkest", w, h, x, y, dark:[x,y,r,g,b,...]} (white image) -> {color}

const input = JSON.parse(await new Promise((r) => {
  let s = '';
  process.stdin.on('data', (d) => (s += d)).on('end', () => r(s));
}));

function area({ rect, key, shift = false, ctrl = false }) {
  let [x0, y0, x1, y1] = rect;
  const step = shift ? 10 : 1;
  const dir = { Left: [-1, 0], Right: [1, 0], Up: [0, -1], Down: [0, 1] }[key];
  if (dir) {
    const [dx, dy] = dir.map((v) => v * step);
    if (ctrl) { x1 += dx; y1 += dy; } else { x0 += dx; x1 += dx; y0 += dy; y1 += dy; }
  } else if (key === ']' || key === '9') {
    x0 -= step; y0 -= step; x1 += step; y1 += step;
  } else if (key === '[' || key === '8') {
    x0 += step; y0 += step; x1 -= step; y1 -= step;
  }
  return { rect: [x0, y0, x1, y1] };
}

// Box: origin = cursor + (-10, -10), size 20x20 (DAT_1002442f8 = -10, DAT_100243f78 = 20).
function darkest({ w, h, x, y, dark }) {
  const px = new Map();
  for (let i = 0; i + 4 < dark.length; i += 5) px.set(`${dark[i]},${dark[i + 1]}`, dark.slice(i + 2, i + 5));
  let best = null, bestL = Infinity;
  for (let yy = Math.max(0, y - 10); yy < Math.min(h, y + 10); yy++) {
    for (let xx = Math.max(0, x - 10); xx < Math.min(w, x + 10); xx++) {
      const p = px.get(`${xx},${yy}`) ?? [255, 255, 255];
      const l = p[0] + p[1] + p[2];
      if (l < bestL) { bestL = l; best = p; }
    }
  }
  const hex = best.map((v) => v.toString(16).toUpperCase().padStart(2, '0')).join('');
  return { color: '#' + hex };
}

const out = input.op === 'area' ? area(input) : darkest(input);
process.stdout.write(JSON.stringify(out));
