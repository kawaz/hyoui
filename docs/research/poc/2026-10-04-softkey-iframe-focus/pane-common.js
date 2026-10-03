// pane 側 (子 frame) の共通処理: focus/blur の通知、キー受信、状態問い合わせへの応答、ログ表示。
// input は focus を監視する要素 (素の textarea か、xterm の helper textarea)。

const params = new URLSearchParams(location.search);
export const paneName = params.get("name") ?? "pane";

const logEl = document.getElementById("log");
const stateEl = document.getElementById("state");
document.getElementById("name").textContent = paneName;

function ts() {
  const d = new Date();
  return `${d.toLocaleTimeString("ja-JP", { hour12: false })}.${String(d.getMilliseconds()).padStart(3, "0")}`;
}

export function log(msg) {
  logEl.textContent = `${ts()} ${msg}\n` + logEl.textContent;
}

function snapshot(input) {
  return {
    hasFocus: document.hasFocus(),
    inputActive: document.activeElement === input,
    active: document.activeElement?.tagName ?? null,
  };
}

function post(msg) {
  parent.postMessage({ ...msg, pane: paneName }, "*");
}

function renderState(input) {
  const s = snapshot(input);
  const on = s.hasFocus && s.inputActive;
  stateEl.textContent = on ? "focus" : `blur (hasFocus=${s.hasFocus} active=${s.active})`;
  stateEl.classList.toggle("on", on);
}

export function connectPane({ input, onKey }) {
  input.addEventListener("focus", () => {
    log("focus");
    renderState(input);
    post({ type: "focus", ...snapshot(input) });
  });
  input.addEventListener("blur", () => {
    log("blur");
    renderState(input);
    post({ type: "blur", ...snapshot(input) });
  });
  // frame 自体の focus 喪失 (= 親側の要素へ focus が移った) も input の blur と同時に来るが、念のため別記録
  window.addEventListener("blur", () => log("window blur"));
  window.addEventListener("focus", () => log("window focus"));

  window.addEventListener("message", (ev) => {
    const m = ev.data;
    if (!m || typeof m !== "object") return;
    if (m.type === "key") {
      const s = snapshot(input);
      log(`key ${m.key} via ${m.method} (hasFocus=${s.hasFocus} inputActive=${s.inputActive})`);
      onKey(m.key);
      post({ type: "ack", seq: m.seq, phase: "recv", ...s });
    } else if (m.type === "query") {
      post({ type: "ack", seq: m.seq, phase: m.phase, ...snapshot(input) });
    }
  });
  renderState(input);
  post({ type: "hello", ...snapshot(input) });
}
