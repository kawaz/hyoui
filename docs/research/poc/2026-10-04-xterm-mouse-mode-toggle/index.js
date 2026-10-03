import { Terminal } from './lib/xterm.mjs';

const host = document.getElementById('host');
const logEl = document.getElementById('log');
const statusEl = document.getElementById('status');

// 方式 (2) の Mac 経路で Option を合成するため macOptionClickForcesSelection を有効にする
const term = new Terminal({ cols: 90, rows: 20, scrollback: 1000, macOptionClickForcesSelection: true });
term.open(host);

// ---- PTY に送られるはずのもの (onData / onBinary) の記録 ----

const MOUSE_REPORT = /^(\x1b\[M[\s\S]{3}|\x1b\[<\d+;\d+;\d+[Mm])$/;
const log = [];
function record(src, s) {
  const entry = { src, s, mouse: src === 'binary' || MOUSE_REPORT.test(s) };
  log.push(entry);
  const line = `${src.padEnd(6)} ${entry.mouse ? 'MOUSE ' : 'data  '} ${JSON.stringify(s)}\n`;
  logEl.textContent += line;
  logEl.scrollTop = logEl.scrollHeight;
}
term.onData(s => record('data', s));
term.onBinary(s => record('binary', s));

// ---- 疑似アプリ: DECSET / DECRST を term.write する ----

const decState = {};
function dec(n, on) {
  decState[n] = on;
  term.write(`\x1b[?${n}${on ? 'h' : 'l'}`);
  for (const b of document.querySelectorAll(`[data-dec="${n}"]`)) b.classList.toggle('on', on);
}
function fill(n = 200, tag = 'line') {
  let s = '';
  for (let i = 1; i <= n; i++) s += `${tag} ${String(i).padStart(3, '0')} abcdefghijklmnopqrstuvwxyz 0123456789\r\n`;
  term.write(s);
}
for (const b of document.querySelectorAll('[data-dec]')) {
  b.addEventListener('click', () => { const n = b.dataset.dec; dec(n, !decState[n]); });
}
document.getElementById('fill').addEventListener('click', () => fill());
document.getElementById('clearlog').addEventListener('click', () => { log.length = 0; logEl.textContent = ''; });

// ---- UI マウスモード (転送 / 遮断) ----

let forward = true;
let method = Number(document.querySelector('input[name=method]:checked').value);
const fwdBtn = document.getElementById('fwd');
function setForward(on) {
  forward = on;
  // 切り替え時に選択を残さない (遮断中の選択が転送再開後に残ると、次の click が選択解除と報告の両方を起こしうる)
  term.clearSelection();
  fwdBtn.textContent = `転送: ${on ? 'オン' : 'オフ'}`;
  fwdBtn.classList.toggle('on', on);
  updateTouchAction();
  renderStatus();
}
function setMethod(m) {
  method = m;
  document.querySelector(`input[name=method][value="${m}"]`).checked = true;
  updateTouchAction();
}
fwdBtn.addEventListener('click', () => setForward(!forward));
for (const r of document.querySelectorAll('input[name=method]')) r.addEventListener('change', () => setMethod(Number(r.value)));
setForward(true);

function requested() { return term.modes.mouseTrackingMode !== 'none'; }
function blocking() { return !forward && requested(); }

function renderStatus() {
  const mode = term.modes.mouseTrackingMode;
  const [cls, label] = !requested() ? ['st-none', '要求なし'] : forward ? ['st-fwd', '要求あり・転送中'] : ['st-block', '要求あり・遮断中'];
  statusEl.innerHTML = `<b class="${cls}">${label}</b> mouseTrackingMode=${mode} buffer=${term.buffer.active.type} DECCKM=${term.modes.applicationCursorKeysMode} method=(${method})`;
  updateTouchAction();
}
term.onWriteParsed(renderStatus);

// ---- 遮断の実装 (host の capture phase、xterm の要素より外側) ----

// 方式 (2) で xterm に渡し直した合成イベント。自分の capture listener で再び止めないための印
const resynth = new WeakSet();
const isMac = ['Macintosh', 'MacIntel', 'MacPPC', 'Mac68K'].includes(navigator.platform);

function stop(ev) {
  ev.stopPropagation();
  ev.preventDefault();
}

host.addEventListener('mousedown', ev => {
  if (resynth.has(ev) || !blocking()) return;
  stop(ev);
  if (method < 2 || ev.button !== 0) return;
  // SelectionService.shouldForceSelection を満たす修飾キーを付けて xterm に渡し直す
  const init = {
    bubbles: true, cancelable: true, composed: true, view: window, detail: ev.detail,
    screenX: ev.screenX, screenY: ev.screenY, clientX: ev.clientX, clientY: ev.clientY,
    button: ev.button, buttons: ev.buttons,
    ctrlKey: ev.ctrlKey, metaKey: ev.metaKey,
    altKey: isMac ? true : ev.altKey,
    shiftKey: isMac ? ev.shiftKey : true,
  };
  const synth = new MouseEvent('mousedown', init);
  resynth.add(synth);
  pendingUp = isMac && ev.altKey;
  ev.target.dispatchEvent(synth);
}, true);

// Mac で本物の Option を押したまま click すると、SelectionService の mouseup が altClickMovesCursor (既定 true) で矢印キー列を onData に流す。合成 mousedown に続く mouseup だけ altKey を外して渡し直す
let pendingUp = false;
document.addEventListener('mouseup', ev => {
  if (resynth.has(ev) || !pendingUp) return;
  pendingUp = false;
  ev.stopPropagation();
  const up = new MouseEvent('mouseup', {
    bubbles: true, cancelable: true, composed: true, view: window, detail: ev.detail,
    screenX: ev.screenX, screenY: ev.screenY, clientX: ev.clientX, clientY: ev.clientY,
    button: ev.button, buttons: ev.buttons,
    ctrlKey: ev.ctrlKey, metaKey: ev.metaKey, shiftKey: ev.shiftKey, altKey: false,
  });
  resynth.add(up);
  ev.target.dispatchEvent(up);
}, true);

host.addEventListener('mousemove', ev => {
  // ボタンを押していない hover (1003 の MOVE 報告) だけ止める。押下中の move は xterm が報告用 listener を結線していない (mousedown を止めたため) ので通し、選択の document listener に届ける
  if (!blocking() || ev.buttons !== 0) return;
  ev.stopPropagation();
}, true);

// ---- 方式 (3): wheel を自前で縦スクロール / 矢印キーに変換 ----

let wheelAcc = 0;
function cellHeight() {
  const screen = host.querySelector('.xterm-screen');
  return screen ? screen.getBoundingClientRect().height / term.rows : 16;
}
function scrollBy(lines) {
  if (lines === 0) return;
  if (term.buffer.active.type === 'normal') {
    term.scrollLines(lines);
  } else {
    // alt screen: xterm がマウス要求なし時に行う変換 (CoreBrowserTerminal の wheel fallback) と同じ列を onData 経路に流す
    const seq = '\x1b' + (term.modes.applicationCursorKeysMode ? 'O' : '[') + (lines < 0 ? 'A' : 'B');
    term.input(seq.repeat(Math.abs(lines)), false);
  }
}
host.addEventListener('wheel', ev => {
  if (!blocking()) return;
  stop(ev);
  if (method < 3) return;
  const px = ev.deltaMode === 1 ? ev.deltaY * cellHeight() : ev.deltaMode === 2 ? ev.deltaY * cellHeight() * term.rows : ev.deltaY;
  wheelAcc += px / cellHeight();
  const lines = Math.trunc(wheelAcc);
  wheelAcc -= lines;
  scrollBy(lines);
}, { capture: true, passive: false });

// ---- 方式 (4): touch の縦スワイプを自前で縦スクロール / 矢印キーに変換、横は scroll-snap に任せる ----

// xterm 6.0.0 は touch で scroll も報告もしない (要求なしでも同じ) ので、TUI に渡す状態 (要求あり・転送中) 以外は自前で縦スクロールを扱う
function ownTouch() { return method >= 4 && !(forward && requested()); }
function updateTouchAction() {
  // 縦パンはブラウザに渡さず自前で扱い、横パン (カルーセル) はブラウザに任せる
  host.classList.toggle('touch-own', ownTouch());
}
let touchY = null;
let touchAcc = 0;
host.addEventListener('touchstart', ev => {
  if (!ownTouch() || ev.touches.length !== 1) { touchY = null; return; }
  touchY = ev.touches[0].clientY;
  touchAcc = 0;
}, { capture: true, passive: true });
host.addEventListener('touchmove', ev => {
  if (touchY === null || !ownTouch()) return;
  const y = ev.touches[0].clientY;
  // 指を上に動かす = 内容を上に送る = 下の行を見る (lines > 0)
  touchAcc += (touchY - y) / cellHeight();
  touchY = y;
  const lines = Math.trunc(touchAcc);
  touchAcc -= lines;
  scrollBy(lines);
}, { capture: true, passive: true });
host.addEventListener('touchend', () => { touchY = null; }, { capture: true, passive: true });

window.poc = { term, log, dec, fill, setForward, setMethod, renderStatus, get forward() { return forward; }, get method() { return method; }, isMac };
