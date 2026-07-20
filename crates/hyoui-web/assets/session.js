// hyoui web — session page.
// Fetches /api/sessions/:id/screen (ANSI) every few seconds, writes bytes into
// an xterm.js instance. Input form POSTs to /api/sessions/:id/input.
(() => {
  const REFRESH_MS = 2000;
  const COLS = 80;
  const ROWS = 24;

  // Extract session id from /sessions/<id>
  const parts = location.pathname.split('/').filter(Boolean);
  const sid = decodeURIComponent(parts[1] || '');
  document.getElementById('sid').textContent = sid;
  document.title = `hyoui — ${sid}`;

  // Embed mode (= ccmsg webui Terminal タブ等の iframe 埋め込み用の軽量ビュー)。
  // ?embed=1 で発火し、header / debug panel を消してターミナル + banner + input のみを
  // viewport にフィットさせる。ページ複製せず同じ session.html を再利用する (DR-0027)。
  const embed = new URLSearchParams(location.search).get('embed') === '1';
  if (embed) {
    document.body.classList.add('embed');
  }

  const term = new Terminal({
    cols: COLS,
    rows: ROWS,
    convertEol: false,
    scrollback: 2000,
    disableStdin: true,
    // 半角ブロック罫線 (▀▄▉ 等) の上下ズレ対策として lineHeight を 1.0 に固定。
    // fontFamily は同梱の HackGen Console NF (SIL OFL、半角:全角=1:2、Nerd Font +
    // 日本語 JIS 第 1-2 水準入り) を最優先。host 依存フォントを消してメトリクス
    // を安定化させる。fallback は既存 monospace 列。
    lineHeight: 1.0,
    fontFamily: '"HackGen Console NF", Menlo, "DejaVu Sans Mono", Consolas, "Courier New", monospace',
    fontSize: 13,
    theme: { background: '#111', foreground: '#e0e0e0' },
  });
  // Unicode 11 addon (絵文字を width=2 として扱う。daemon 側 vt100 emulator と一致)。
  if (window.Unicode11Addon) {
    try {
      const addon = new window.Unicode11Addon.Unicode11Addon();
      term.loadAddon(addon);
      term.unicode.activeVersion = '11';
      if (window.__hyouiDebug) window.__hyouiDebug('info', 'unicode11 addon loaded, activeVersion=' + term.unicode.activeVersion);
    } catch (e) {
      if (window.__hyouiDebug) window.__hyouiDebug('warn', 'unicode11 addon load failed: ' + e.message);
    }
  } else if (window.__hyouiDebug) {
    window.__hyouiDebug('warn', 'Unicode11Addon global not found');
  }
  // Fit addon (コンテナサイズ → cols/rows を計算)。
  let fitAddon = null;
  if (window.FitAddon) {
    try {
      fitAddon = new window.FitAddon.FitAddon();
      term.loadAddon(fitAddon);
      if (window.__hyouiDebug) window.__hyouiDebug('info', 'fit addon loaded');
    } catch (e) {
      if (window.__hyouiDebug) window.__hyouiDebug('warn', 'fit addon load failed: ' + e.message);
    }
  } else if (window.__hyouiDebug) {
    window.__hyouiDebug('warn', 'FitAddon global not found');
  }
  term.open(document.getElementById('term'));

  const statusEl = document.getElementById('status');
  const sizeEl = document.getElementById('size');
  const autoEl = document.getElementById('auto');
  const autoResizeEl = document.getElementById('autoResize');
  const refreshBtn = document.getElementById('refresh');
  const inputForm = document.getElementById('inputForm');
  const inputText = document.getElementById('inputText');
  const sendRawBtn = document.getElementById('sendRaw');
  const sendKeyBtn = document.getElementById('sendKey');
  const sendStatus = document.getElementById('sendStatus');
  const stoppedBanner = document.getElementById('stoppedBanner');
  const resumeBtn = document.getElementById('resumeBtn');
  // HTML の `hidden` 属性に加え script 側でも明示的に hide しておく (belt-and-suspenders)。
  // 初回 fetch が成功して child_stopped=true と判明するまでは絶対に表示させない
  // (= false-positive で banner が一瞬映る過渡を潰す。CSS 側の override も別途施した)。
  stoppedBanner.hidden = true;

  let timer = null;
  let lastPayload = '';
  let lastCols = COLS;
  let lastRows = ROWS;

  // ---- resize logic (kawaz 要望 2026-07-21) ----
  //
  // 責務: (a) 表示 fit (= xterm.js の grid を viewport に合わせる) は default on。
  //       (b) daemon 側 PTY の resize (= 実 TUI 再レイアウト) は明示 opt-in
  //           (autoResizeEl checkbox、localStorage 保持、default off)。
  //
  // なぜ (b) が opt-in か: iframe 埋め込み (ccmsg webui Terminal タブ等) で
  // 意図せず PTY resize が発火して稼働中の TUI (claude / vim) を勝手に
  // 再レイアウトさせないため。
  const LS_AUTO_RESIZE = 'hyoui.session.autoResize';
  try {
    autoResizeEl.checked = localStorage.getItem(LS_AUTO_RESIZE) === '1';
  } catch (_e) { /* private mode 等で失敗しても既定 off */ }
  autoResizeEl.addEventListener('change', () => {
    try { localStorage.setItem(LS_AUTO_RESIZE, autoResizeEl.checked ? '1' : '0'); } catch (_e) {}
    // opt-in した瞬間に現サイズを一度 PTY に反映。
    if (autoResizeEl.checked) sendResizeIfChanged(true);
  });

  async function sendResizeIfChanged(force) {
    const cols = term.cols;
    const rows = term.rows;
    if (!force && cols === lastCols && rows === lastRows) return;
    lastCols = cols;
    lastRows = rows;
    if (sizeEl) sizeEl.textContent = `${cols}x${rows}`;
    if (!autoResizeEl.checked) return;
    try {
      const r = await fetch(`/api/sessions/${encodeURIComponent(sid)}/resize`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ cols, rows }),
      });
      if (!r.ok) {
        const txt = await r.text();
        if (window.__hyouiDebug) window.__hyouiDebug('warn', `resize HTTP ${r.status}: ${txt}`);
        return;
      }
      // resize 後は daemon が新サイズで redraw するので screen を取り直す。
      setTimeout(fetchScreen, 200);
    } catch (e) {
      if (window.__hyouiDebug) window.__hyouiDebug('warn', 'resize failed: ' + e.message);
    }
  }

  let fitTimer = null;
  function scheduleFit() {
    if (!fitAddon) return;
    if (fitTimer) clearTimeout(fitTimer);
    fitTimer = setTimeout(() => {
      fitTimer = null;
      try {
        fitAddon.fit();
        sendResizeIfChanged(false);
      } catch (e) {
        if (window.__hyouiDebug) window.__hyouiDebug('warn', 'fit failed: ' + e.message);
      }
    }, 150); // debounce (数百 ms オーダで十分、resize drag 中の連射を抑える)
  }

  // 初回 fit は open() 直後の layout 完了を待って 1 tick 後に。
  setTimeout(() => {
    if (fitAddon) {
      try { fitAddon.fit(); } catch (_e) {}
    }
    lastCols = term.cols;
    lastRows = term.rows;
    if (sizeEl) sizeEl.textContent = `${term.cols}x${term.rows}`;
  }, 0);

  window.addEventListener('resize', scheduleFit);
  if (typeof ResizeObserver !== 'undefined') {
    try {
      new ResizeObserver(scheduleFit).observe(document.getElementById('term'));
    } catch (_e) { /* best-effort */ }
  }

  async function refreshSessionStatus() {
    // /api/sessions を一覧して自分の session_id を探し child_stopped で banner を出す。
    // 専用エンドポイントを増やさず既存 API を再利用 (= protocol/API 表面を最小化)。
    try {
      const r = await fetch('/api/sessions', { cache: 'no-store' });
      if (!r.ok) return;
      const list = await r.json();
      const me = Array.isArray(list) ? list.find((s) => s.session_id === sid) : null;
      const stopped = !!(me && me.child_stopped);
      stoppedBanner.hidden = !stopped;
    } catch (_e) {
      // best-effort。失敗しても画面は動かす。
    }
  }

  async function sendResume() {
    resumeBtn.disabled = true;
    const orig = resumeBtn.textContent;
    resumeBtn.textContent = 'resuming…';
    try {
      const r = await fetch(`/api/sessions/${encodeURIComponent(sid)}/resume`, { method: 'POST' });
      if (!r.ok) {
        const txt = await r.text();
        throw new Error(`HTTP ${r.status}: ${txt}`);
      }
      // 復帰後 daemon が redraw を送るので、screen と status の両方を fetch し直す。
      setTimeout(() => { fetchScreen(); refreshSessionStatus(); }, 300);
    } catch (e) {
      alert('resume failed: ' + e.message);
    } finally {
      resumeBtn.disabled = false;
      resumeBtn.textContent = orig;
    }
  }
  resumeBtn.addEventListener('click', sendResume);

  async function fetchScreen() {
    statusEl.textContent = 'fetching…';
    try {
      const r = await fetch(`/api/sessions/${encodeURIComponent(sid)}/screen`, { cache: 'no-store' });
      if (r.status === 404) {
        statusEl.textContent = 'session not found (404)';
        return;
      }
      if (!r.ok) throw new Error('HTTP ' + r.status);
      const text = await r.text();
      if (text !== lastPayload) {
        // Full redraw: reset and write the full ANSI dump.
        term.reset();
        term.write(text, () => {
          if (window.__hyouiDebug) {
            const line = term.buffer.active.getLine(0);
            window.__hyouiDebug('screen', `wrote ${text.length}B, buffer line0="${line ? line.translateToString().trimEnd().slice(0, 40) : '(null)'}"`);
          }
        });
        lastPayload = text;
      }
      statusEl.textContent = `updated ${new Date().toLocaleTimeString()} (${text.length} B)`;
    } catch (e) {
      statusEl.textContent = 'error: ' + e.message;
    }
  }

  async function sendSpecs(specs) {
    sendStatus.textContent = 'sending…';
    try {
      const r = await fetch(`/api/sessions/${encodeURIComponent(sid)}/input`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ specs }),
      });
      const txt = await r.text();
      if (!r.ok) throw new Error(`HTTP ${r.status}: ${txt}`);
      sendStatus.textContent = `sent (${txt})`;
      // Refresh screen soon after sending.
      setTimeout(fetchScreen, 300);
    } catch (e) {
      sendStatus.textContent = 'send error: ' + e.message;
    }
  }

  inputForm.addEventListener('submit', (ev) => {
    ev.preventDefault();
    const t = inputText.value;
    const specs = [];
    if (t.length > 0) specs.push('text:' + t);
    specs.push('key:Enter');
    sendSpecs(specs);
    inputText.value = '';
  });

  sendRawBtn.addEventListener('click', () => {
    const t = inputText.value;
    if (t.length === 0) return;
    sendSpecs(['text:' + t]);
    inputText.value = '';
  });

  // 特殊キーパッド (iPad ソフトキーボードで打てない矢印 / Tab / Esc / Ctrl-C 等)。
  // 各 button の data-spec を POST /input の spec としてそのまま送る。
  // mousedown で preventDefault することで button に focus を奪われず、送信後も
  // input の focus はそのまま (= 連打しやすさ)。
  const keypad = document.getElementById('keypad');
  if (keypad) {
    keypad.querySelectorAll('button[data-spec]').forEach((btn) => {
      btn.addEventListener('mousedown', (e) => e.preventDefault());
      btn.addEventListener('click', () => {
        const spec = btn.getAttribute('data-spec');
        if (spec) sendSpecs([spec]);
      });
    });
  }

  sendKeyBtn.addEventListener('click', () => {
    const name = prompt('Key name (e.g. Escape, Tab, Ctrl-C, C-a):');
    if (!name) return;
    sendSpecs(['key:' + name]);
  });

  function schedule() {
    if (timer) { clearInterval(timer); timer = null; }
    if (autoEl.checked) {
      timer = setInterval(fetchScreen, REFRESH_MS);
    }
  }

  autoEl.addEventListener('change', schedule);
  refreshBtn.addEventListener('click', () => { fetchScreen(); refreshSessionStatus(); });
  // PWA (standalone) 用の全ページリロード。詳細は index.js の該当箇所参照。
  const reloadBtn = document.getElementById('reload');
  if (reloadBtn) reloadBtn.addEventListener('click', () => location.reload());
  fetchScreen();
  refreshSessionStatus();
  schedule();
  // status は screen より遅めに poll (= 一覧 API を頻繁に叩かない)。
  setInterval(refreshSessionStatus, 5000);
})();
