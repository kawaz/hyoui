import { Terminal } from '../2026-10-04-xterm-mouse-mode-toggle/lib/xterm.mjs';

const params = new URLSearchParams(location.search);
const OPTS = {
  mode: { values: ['A', 'B0', 'B1', 'none'], def: 'A' },
  wrap: { values: ['join', 'split'], def: 'join' },
  freeze: { values: ['on', 'off'], def: 'on' },
  show: { values: ['0', '1'], def: '0' },
  focus: { values: ['collapsed', 'always'], def: 'collapsed' },
  fit: { values: ['1', '0'], def: '1' },
};
const opt = {};
for (const [k, o] of Object.entries(OPTS)) {
  const v = params.get(k);
  opt[k] = o.values.includes(v) ? v : o.def;
  const box = document.getElementById(`${k}Links`);
  for (const value of o.values) {
    const p = new URLSearchParams(location.search);
    p.set(k, value);
    const a = document.createElement('a');
    a.href = `?${p}`;
    a.textContent = value;
    if (value === opt[k]) a.className = 'cur';
    box.appendChild(a);
  }
}

const host = document.getElementById('host');
const statusEl = document.getElementById('status');
const logEl = document.getElementById('log');

const term = new Terminal({
  cols: 40,
  rows: 24,
  scrollback: 1000,
  fontSize: 16,
  fontFamily: 'Menlo, ui-monospace, monospace',
  cursorBlink: false,
});
term.open(host);

const log = [];
function record(src, s) {
  log.push({ src, s });
  logEl.textContent += `${src.padEnd(6)} ${JSON.stringify(s)}\n`;
  logEl.scrollTop = logEl.scrollHeight;
}
term.onData(s => record('data', s));
term.onBinary(s => record('binary', s));

// ---- テストデータ ----

const ESC = '\x1b';
const DATA = [
  { id: '1-wrap', label: '折り返す長い 1 行 (英字・全角混在)', s: 'The quick brown fox jumps over the lazy dog. 日本語の全角文字を混ぜて折り返す長い一行です。END' },
  { id: '2a-trail-sp', label: '行末にスペースを 5 個書いた行', s: 'trail-sp     ' },
  { id: '2b-trail-none', label: '行末に何も書いていない行 (2a と同じ見た目)', s: 'trail-sp' },
  { id: '3a-box-sp', label: '罫線の枠 (内側の右側はスペースを書く)', s: `┌──────────┐\r\n│abc       │\r\n└──────────┘` },
  { id: '3b-box-cuf', label: '罫線の枠 (内側の右側は CUF で飛ばす)', s: `┌──────────┐\r\n│abc${ESC}[7C│\r\n└──────────┘` },
  { id: '4a-bg-sp', label: '背景色 + スペースを書いた領域 (行末)', s: `bg-sp:${ESC}[44m          ${ESC}[0m` },
  { id: '4b-bg-el', label: '背景色 + EL で消した領域 (行末、セルは未書き込みで bg だけ持つ)', s: `bg-el:${ESC}[44m${ESC}[K${ESC}[0m` },
  { id: '4c-none', label: '何も書いていない領域 (行末)', s: 'none:' },
  { id: '5-wide', label: '全角・曖昧幅・絵文字・結合文字', s: '全角あいう ○①★ 😀✅ é end' },
  { id: '6-cuf', label: '行の途中を CUF 10 で飛ばした未書き込みセル', s: `cuf:${ESC}[10Cafter` },
];

function write(s) { return new Promise(resolve => term.write(s, resolve)); }

async function writeData() {
  for (const item of DATA) {
    const b = term.buffer.active;
    item.row = b.baseY + b.cursorY;
    await write(`${item.s}\r\n`);
    const b2 = term.buffer.active;
    item.rows = b2.baseY + b2.cursorY - item.row;
  }
}

// ---- 共通: セル寸法 ----

function screenEl() { return host.querySelector('.xterm-screen'); }
function cellMetrics() {
  const r = screenEl().getBoundingClientRect();
  return { left: r.left, top: r.top, w: r.width / term.cols, h: r.height / term.rows };
}

// ---- (A) クローン層 ----

let layer = null;
let layerDirty = false;

// 1 行分のセル列。public API だけで作る。未書き込みセル (getChars() === '') は
// 行の途中ならスペース、最後の書き込み済みセルより右なら空の箱 (xterm の translateToString(true) と同じ扱い)
function rowCells(line, cols) {
  const cells = [];
  let lastContent = -1;
  for (let x = 0; x < cols; x++) {
    const cell = line.getCell(x);
    if (!cell) break;
    if (cell.getChars() !== '') lastContent = x;
    cells.push({ x, width: cell.getWidth(), chars: cell.getChars() });
  }
  return cells.filter(c => c.width !== 0).map(c => ({
    ...c,
    text: c.chars !== '' ? c.chars : (c.x < lastContent ? ' ' : ''),
  }));
}

// fit=1: 文字の送り幅をセル幅に揃える (xterm の DOM renderer の letter-spacing と同じ考え方、DomRendererRowFactory.ts の spacing 計算)。
// 揃えないと全角のグリフ (16px 等) が 2 セル (19.25px) より狭く、選択の当たり判定 (グリフの中点) とハイライトの右端がセルとずれる
const measureCtx = document.createElement('canvas').getContext('2d');
const glyphWidthCache = new Map();
function glyphWidth(s) {
  let w = glyphWidthCache.get(s);
  if (w === undefined) {
    measureCtx.font = `${term.options.fontSize}px ${term.options.fontFamily}`;
    w = measureCtx.measureText(s).width;
    glyphWidthCache.set(s, w);
  }
  return w;
}

function buildLayer() {
  if (!layer) return;
  const m = cellMetrics();
  const b = term.buffer.active;
  const frag = document.createDocumentFragment();
  let blk = null;
  for (let y = 0; y < term.rows; y++) {
    const line = b.getLine(b.viewportY + y);
    if (!line) continue;
    if (!blk || opt.wrap === 'split' || !line.isWrapped) {
      blk = document.createElement('div');
      blk.className = 'blk';
      blk.dataset.y = String(y);
      blk.style.width = `${term.cols * m.w}px`;
      frag.appendChild(blk);
    }
    for (const c of rowCells(line, term.cols)) {
      const span = document.createElement('span');
      span.className = 'c';
      span.dataset.x = String(c.x);
      span.dataset.y = String(y);
      span.style.width = `${c.width * m.w}px`;
      span.style.height = `${m.h}px`;
      span.style.lineHeight = `${m.h}px`;
      span.textContent = c.text;
      if (opt.fit === '1' && c.text !== '') {
        const spacing = c.width * m.w - glyphWidth(c.text);
        if (Math.abs(spacing) > 0.01) span.style.letterSpacing = `${spacing}px`;
      }
      blk.appendChild(span);
    }
  }
  layer.style.width = `${term.cols * m.w}px`;
  layer.style.font = `${term.options.fontSize}px ${term.options.fontFamily}`;
  layer.replaceChildren(frag);
  layerDirty = false;
}

function selectionInLayer() {
  const sel = document.getSelection();
  return sel && !sel.isCollapsed && layer && layer.contains(sel.anchorNode);
}

function requestLayer() {
  if (opt.freeze === 'on' && selectionInLayer()) { layerDirty = true; return; }
  buildLayer();
}

function installA() {
  layer = document.createElement('div');
  layer.className = `sel-layer${opt.show === '1' ? ' show' : ''}`;
  screenEl().appendChild(layer);
  // 選択用のポインタ操作は層で受け、xterm (選択・マウス報告・textarea 移動) へは渡さない
  for (const t of ['mousedown', 'mouseup', 'click', 'dblclick', 'contextmenu', 'auxclick']) {
    layer.addEventListener(t, e => e.stopPropagation());
  }
  term.onRender(requestLayer);
  term.onScroll(requestLayer);
  document.addEventListener('selectionchange', () => {
    if (layerDirty && !selectionInLayer()) buildLayer();
  });
  buildLayer();
}

// ---- (B) xterm の行 DOM を選択可能に ----

// B0: CSS の user-select だけ。B1: + 行 DOM に pointer-events を戻し、xterm の mousedown を host の capture で止める
function installB() {
  document.body.classList.add('b-select');
  if (opt.mode === 'B1') {
    document.body.classList.add('b1');
    // xterm の mousedown (preventDefault + 自前選択開始) を止め、ブラウザ標準の選択に任せる
    host.addEventListener('mousedown', e => {
      if (e.target instanceof Node && screenEl().contains(e.target)) e.stopPropagation();
    }, true);
  }
}

// 層 / 行 DOM で選択した後、端末にフォーカスを戻す (キー入力を xterm へ)。
// collapsed: 何も選ばれていない click の時だけ。always: ドラッグ選択の後も
function installFocusReturn() {
  host.addEventListener('mouseup', () => {
    const sel = document.getSelection();
    if (opt.focus === 'always' || !sel || sel.isCollapsed) term.focus();
  }, true);
}

// ---- 計測 (Playwright / devtools から呼ぶ) ----

// 行 DOM (xterm の DOM renderer) と層の各セルの x 座標を、期待位置 (screen 左端 + x * セル幅) と比べる
function textNodesOf(el) {
  const out = [];
  const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
  for (let n = walker.nextNode(); n; n = walker.nextNode()) out.push(n);
  return out;
}
function rangeRect(node, start, end) {
  const r = document.createRange();
  r.setStart(node, start);
  r.setEnd(node, end);
  return r.getBoundingClientRect();
}

function alignment(y) {
  const m = cellMetrics();
  const b = term.buffer.active;
  const line = b.getLine(b.viewportY + y);
  const rowEl = host.querySelectorAll('.xterm-rows > div')[y];
  // xterm 行 DOM のテキストを (node, offset) 列に平らにする
  const flat = [];
  for (const n of textNodesOf(rowEl)) for (let i = 0; i < n.data.length; i++) flat.push([n, i]);
  let p = 0;
  const cells = [];
  for (let x = 0; x < term.cols; x++) {
    const cell = line.getCell(x);
    if (!cell || cell.getWidth() === 0) continue;
    const chars = cell.getChars() || ' ';
    const expL = m.left + x * m.w;
    const rec = { x, chars, width: cell.getWidth(), exp: expL };
    if (p + chars.length <= flat.length) {
      const [n0, o0] = flat[p];
      const [n1, o1] = flat[p + chars.length - 1];
      if (n0 === n1) {
        const r = rangeRect(n0, o0, o1 + 1);
        rec.xterm = r.left - expL;
        rec.xtermW = r.width;
      }
      p += chars.length;
    }
    if (layer) {
      const span = layer.querySelector(`.c[data-y="${y}"][data-x="${x}"]`);
      if (span) {
        rec.layerBox = span.getBoundingClientRect().left - expL;
        if (span.firstChild) {
          const r = rangeRect(span.firstChild, 0, span.firstChild.data.length);
          rec.layerGlyph = r.left - expL;
          rec.layerGlyphW = r.width;
        }
      }
    }
    cells.push(rec);
  }
  return { cw: m.w, ch: m.h, cells };
}

function rowTops() {
  const m = cellMetrics();
  const out = [];
  const rows = host.querySelectorAll('.xterm-rows > div');
  for (let y = 0; y < term.rows; y++) {
    const rec = { y, exp: m.top + y * m.h - m.top };
    if (rows[y]) rec.xterm = rows[y].getBoundingClientRect().top - m.top;
    const span = layer?.querySelector(`.c[data-y="${y}"]`);
    if (span) rec.layer = span.getBoundingClientRect().top - m.top;
    out.push(rec);
  }
  return out;
}

function updateStatus() {
  statusEl.textContent = `mode=${opt.mode} wrap=${opt.wrap} freeze=${opt.freeze} cols=${term.cols} rows=${term.rows} mouseTrackingMode=${term.modes.mouseTrackingMode} xtermSelection=${JSON.stringify(term.getSelection())}`;
}

// ---- 起動 ----

let tickN = 0;
document.getElementById('tick').addEventListener('click', () => { term.write(`tick ${++tickN}\r\n`); });
let mouseOn = false;
document.getElementById('mouse1002').addEventListener('click', () => {
  mouseOn = !mouseOn;
  term.write(mouseOn ? `${ESC}[?1002h${ESC}[?1006h` : `${ESC}[?1002l${ESC}[?1006l`);
});
const paste = document.getElementById('paste');
paste.addEventListener('input', () => { document.getElementById('pasteJson').textContent = JSON.stringify(paste.value); });
term.onRender(updateStatus);
term.onSelectionChange(updateStatus);

await writeData();
if (opt.mode === 'A') installA();
if (opt.mode === 'B0' || opt.mode === 'B1') installB();
if (opt.mode !== 'none') installFocusReturn();
updateStatus();

window.__poc = { term, opt, DATA, log, cellMetrics, alignment, rowTops, buildLayer, get layer() { return layer; }, write, ready: true };
