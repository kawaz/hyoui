// 親 frame: ソフトキーパネルのボタン方式ごとに、タップ後も子 pane の入力要素に focus が残るかを記録する。
//
// 判定: 1 回のタップ (gesture) ごとに seq を振り、子へ key を送った時 (recv)、指を離した時 (up)、click 時 (click)、
// その 2 フレーム後 (settled) に子の focus 状態を問い合わせる。全時点で対象 pane の入力要素が active かつ
// document.hasFocus() なら OK、1 つでも外れていれば NG。子からの blur 通知も log に出る。

const params = new URLSearchParams(location.search);
const paneB = params.get("b") === "textarea" ? "pane.html" : "pane-xterm.html";
document.getElementById("frame-B").src = `${paneB}?name=B`;

const frames = { A: document.getElementById("frame-A"), B: document.getElementById("frame-B") };
const logEl = document.getElementById("log");
const panel = document.getElementById("panel");

function ts() {
  const d = new Date();
  return `${d.toLocaleTimeString("ja-JP", { hour12: false })}.${String(d.getMilliseconds()).padStart(3, "0")}`;
}
function log(msg) {
  logEl.textContent = `${ts()} ${msg}\n` + logEl.textContent;
}
document.getElementById("clear").addEventListener("click", (e) => {
  e.preventDefault();
  logEl.textContent = "";
});

// --- focus 状態の追跡 ---------------------------------------------------------

const paneFocus = { A: false, B: false };
let lastPane = null;

function renderFocus() {
  const focused = Object.keys(paneFocus).filter((k) => paneFocus[k]);
  const ae = document.activeElement;
  const parentSide = ae && ae.tagName !== "IFRAME" && ae !== document.body ? `parent:${ae.tagName}${ae.dataset.method ? `(${ae.dataset.method})` : ""}` : null;
  document.getElementById("st-focus").textContent = focused.length ? focused.join(",") : parentSide ?? "なし";
  document.getElementById("st-last").textContent = lastPane ?? "-";
  for (const [k, f] of Object.entries(frames)) f.classList.toggle("last", k === lastPane);
}
document.addEventListener("focusin", renderFocus);
document.addEventListener("focusout", () => queueMicrotask(renderFocus));

// --- ソフトキーボード表示の推定 (visualViewport) ----------------------------------

const vv = window.visualViewport;
const baseline = {}; // 向きごとの visualViewport.height 最大値 = キーボード非表示時の高さとみなす
const KBD_THRESHOLD = 120; // px。iOS のキーボードは最小でも 200px 超、アドレスバー伸縮 (~60-80px) とは区別できる幅

function onViewport() {
  const orient = innerWidth > innerHeight ? "landscape" : "portrait";
  baseline[orient] = Math.max(baseline[orient] ?? 0, vv.height);
  const shrink = baseline[orient] - vv.height;
  document.getElementById("st-kbd").textContent = shrink > KBD_THRESHOLD ? `表示 (-${Math.round(shrink)}px)` : "非表示";
  document.getElementById("st-vv").textContent = `vv.h=${Math.round(vv.height)} base=${Math.round(baseline[orient])} innerH=${innerHeight}`;
  // panel を visual viewport の下端に追従させる
  panel.style.bottom = `${Math.max(0, innerHeight - vv.height - vv.offsetTop)}px`;
}
vv.addEventListener("resize", onViewport);
vv.addEventListener("scroll", onViewport);
onViewport();

// --- gesture 単位の判定 ---------------------------------------------------------

let seq = 0;
const gestures = new Map(); // seq -> { method, pane, acks: {phase: snapshot}, resEl }

function sendToPane(pane, msg) {
  frames[pane]?.contentWindow?.postMessage(msg, "*");
}

function startGesture(method, key, resEl) {
  const s = ++seq;
  const g = { method, key, pane: lastPane, acks: {}, resEl, blurred: false };
  gestures.set(s, g);
  current = s;
  if (!g.pane) {
    log(`#${s} ${method} ${key}: 送信先 pane なし (先に pane をタップ)`);
    return s;
  }
  sendToPane(g.pane, { type: "key", key, method, seq: s });
  return s;
}
let current = 0;

function query(phase) {
  const s = current;
  const g = gestures.get(s);
  if (!g?.pane) return;
  sendToPane(g.pane, { type: "query", seq: s, phase });
}

function querySettled() {
  requestAnimationFrame(() => requestAnimationFrame(() => query("settled")));
}

function judge(s) {
  const g = gestures.get(s);
  const phases = Object.entries(g.acks);
  const ok = !g.blurred && phases.every(([, a]) => a.hasFocus && a.inputActive);
  g.resEl.textContent = ok ? "OK" : "NG";
  g.resEl.className = `res ${ok ? "ok" : "ng"}`;
  const detail = phases.map(([p, a]) => `${p}:${a.hasFocus && a.inputActive ? "o" : `x(${a.active},hasFocus=${a.hasFocus})`}`).join(" ");
  return { ok, detail };
}

window.addEventListener("message", (ev) => {
  const m = ev.data;
  if (!m || typeof m !== "object" || !m.pane) return;
  if (m.type === "focus") {
    paneFocus[m.pane] = true;
    lastPane = m.pane;
    log(`pane ${m.pane} focus`);
  } else if (m.type === "blur") {
    paneFocus[m.pane] = false;
    const g = gestures.get(current);
    if (g && g.pane === m.pane) {
      g.blurred = true;
      judge(current);
    }
    log(`pane ${m.pane} blur (gesture #${current} 中)`);
  } else if (m.type === "hello") {
    log(`pane ${m.pane} loaded`);
  } else if (m.type === "ack") {
    const g = gestures.get(m.seq);
    if (!g) return;
    g.acks[m.phase] = m;
    const { ok, detail } = judge(m.seq);
    if (m.phase === "settled") log(`#${m.seq} ${g.method} ${g.key} -> ${g.pane}: ${ok ? "OK" : "NG"} [${detail}]${g.blurred ? " blur発生" : ""}`);
  }
  renderFocus();
});

// --- ボタン方式 ---------------------------------------------------------------

const KEYS = ["Esc", "Tab", "^C", "←", "→"];

// 方式ごとの listener の付け方。id は log / 判定表示用
function bindClick(id) {
  return (el, key, resEl) => {
    el.addEventListener("click", () => {
      startGesture(id, key, resEl);
      query("click");
      querySettled();
    });
  };
}

function bindPointerPrevent(id) {
  return (el, key, resEl) => {
    el.addEventListener("pointerdown", (e) => {
      e.preventDefault();
      startGesture(id, key, resEl);
    });
    // touch 端末では pointerdown の preventDefault で click が来ないことがあるため settled は pointerup 起点
    el.addEventListener("pointerup", () => {
      query("up");
      querySettled();
    });
    el.addEventListener("click", () => query("click"));
  };
}

function bindMouseTouchPrevent(id) {
  return (el, key, resEl) => {
    // touch 端末では touchstart → (互換) mousedown の順に来るので、touchstart 直後の mouse 系は二重処理しない
    let lastTouch = -Infinity;
    const recentTouch = () => performance.now() - lastTouch < 1000;
    el.addEventListener("touchstart", (e) => {
      e.preventDefault(); // iOS ではこれで互換 mouse イベントと click も抑止される
      lastTouch = performance.now();
      startGesture(id, key, resEl);
    }, { passive: false });
    el.addEventListener("touchend", () => {
      query("up");
      querySettled();
    });
    el.addEventListener("mousedown", (e) => {
      e.preventDefault();
      if (!recentTouch()) startGesture(id, key, resEl);
    });
    el.addEventListener("mouseup", () => {
      if (recentTouch()) return;
      query("up");
      querySettled();
    });
    el.addEventListener("click", () => query("click"));
  };
}

const METHODS = [
  { id: "a", label: "(a) button + click", make: () => document.createElement("button"), bind: bindClick("a") },
  { id: "b", label: "(b) pointerdown preventDefault", make: () => document.createElement("button"), bind: bindPointerPrevent("b") },
  { id: "c", label: "(c) mousedown/touchstart preventDefault", make: () => document.createElement("button"), bind: bindMouseTouchPrevent("c") },
  {
    id: "d",
    label: "(d) tabindex=-1 + pointerdown pD",
    make: () => {
      const b = document.createElement("button");
      b.tabIndex = -1;
      return b;
    },
    bind: bindPointerPrevent("d"),
  },
  {
    // 参考: focus 不可能な要素 (div) なら preventDefault 無しでも focus が移らないかの比較
    id: "e",
    label: "(e) div role=button + click",
    make: () => {
      const d = document.createElement("div");
      d.setAttribute("role", "button");
      return d;
    },
    bind: bindClick("e"),
  },
];

for (const m of METHODS) {
  const row = document.createElement("div");
  row.className = "row";
  const label = document.createElement("span");
  label.className = "label";
  label.textContent = m.label;
  const res = document.createElement("span");
  res.className = "res";
  res.textContent = "-";
  row.append(label);
  for (const key of KEYS) {
    const el = m.make();
    el.className = "key";
    el.textContent = key;
    el.dataset.method = m.id;
    m.bind(el, key, res);
    row.append(el);
  }
  row.append(res);
  panel.append(row);
}

renderFocus();
