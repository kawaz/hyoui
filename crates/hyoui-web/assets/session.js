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
    // DR-0027 Phase 3: WS attach 接続後は raw stream で xterm.js が直接キー入力を
    // WS に流すため、stdin を有効化する。WS 未接続時のフォーカス入力は WS が
    // 復活するまで無害 (= term.onData で input queue に貯まるが送り先が無いので
    // WS 接続時に flush される。UX 上は input 欄経由が primary な fallback)。
    disableStdin: false,
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
  // playwright / devtools 用の debug hook。production でも露出しているが
  // xterm.js の内部 API なので副作用は限定的 (= UI ボタンを増やす代わり)。
  window.__hyouiTerm = term;

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
  // ?resize=1 で auto-resize を強制 ON (embed では header ごとトグルが消える +
  // iframe の third-party storage 分離で localStorage が親ページと共有されない
  // 環境があるため、URL で明示 opt-in できる経路を用意する)。
  if (new URLSearchParams(location.search).get('resize') === '1') {
    autoResizeEl.checked = true;
  }
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
    // auto-resize が既に ON (localStorage / ?resize=1) なら初回サイズを即 PTY へ。
    if (autoResizeEl.checked) sendResizeIfChanged(true);
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

  // fetchScreen は「reset + 全 ANSI 書き直し」を行うため、実行するたびに
  // scrollback 履歴と現行の選択範囲を破棄する。WS attach 中は raw stream の
  // incremental append で状態が反映され続けるので、fetchScreen を呼ぶと
  // scrollback / 選択 / コピー中の状態が壊れる (kawaz 受け入れ条件 2026-07-23)。
  // よって wsIsOpen() 判定で reset 系呼び出しは全て抑止する:
  // - refresh ボタン: WS 中は no-op に降格 (`refresh (ws active)` バッジで示す)
  // - auto refresh checkbox: WS 中は無効化 (schedule() が timer を張らない)
  // - sendSpecs 後の 300ms 後 fetchScreen: WS 中は skip (WS 側で描画される)
  // WS onopen 直後の初回 fetchScreen だけは、bridge が接続時点以降の追記しか
  // 出さない性質上、初期状態を xterm へ流し込む唯一の手段なので許容する
  // (= 直後に選択操作は無いはず、以降は WS のみ)。
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
      // Refresh screen soon after sending — WS 中は bridge が echo を stream で
      // 返すので fetchScreen を呼ばない (reset で scrollback / 選択が消えるため)。
      if (!wsIsOpen()) setTimeout(fetchScreen, 300);
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
    // WS 中は reset を伴う fetchScreen をタイマーで走らせない (受け入れ条件)。
    if (autoEl.checked && !wsIsOpen()) {
      timer = setInterval(fetchScreen, REFRESH_MS);
    }
  }

  // ---- DR-0027 Phase 3: WebSocket attach (フルターミナル bridge) ----
  //
  // WS 接続確立中は screen ポーリングを止め、xterm.js の onData / onBinary で
  // 生成した raw bytes を WS に投げ、daemon から返ってくる raw stream を
  // term.write でリアルタイム描画する。WS 切断時は fallback として screen
  // ポーリング + input 欄経路に戻り、指数バックオフで再接続を試みる。
  let ws = null;
  let wsReconnectMs = 1000;
  const WS_RECONNECT_MAX_MS = 30000;
  let wsExplicitClose = false; // ページ離脱等の意図的 close を示す flag
  // WS 接続前 (未開通 / 再接続中) に xterm.onData で来た入力は queue して、
  // 接続復帰時に送る。過剰蓄積を防ぐため上限 8 KiB (= 短時間の間 typing 分は
  // 拾えるが、切断状態で長時間叩き続けても memory は伸びない)。
  const wsPendingInput = [];
  let wsPendingBytes = 0;
  const WS_PENDING_MAX = 8 * 1024;

  // 接続状態バッジ (existing status 領域の右側に別 element を新設)。
  const wsStatusEl = document.createElement('span');
  wsStatusEl.id = 'wsStatus';
  wsStatusEl.className = 'meta';
  wsStatusEl.style.marginLeft = '0.5em';
  wsStatusEl.textContent = 'ws: init';
  if (statusEl && statusEl.parentNode) {
    statusEl.parentNode.appendChild(document.createTextNode(' '));
    statusEl.parentNode.appendChild(wsStatusEl);
  }
  function setWsStatus(text) {
    wsStatusEl.textContent = 'ws: ' + text;
  }

  function wsIsOpen() { return ws && ws.readyState === WebSocket.OPEN; }

  function flushPendingToWs() {
    if (!wsIsOpen() || wsPendingInput.length === 0) return;
    for (const b of wsPendingInput) {
      try { ws.send(b); } catch (_e) { break; }
    }
    wsPendingInput.length = 0;
    wsPendingBytes = 0;
  }

  function sendBytesToWs(bytes) {
    // bytes は Uint8Array or string (xterm.js onData は string、onBinary は Uint8Array)
    if (wsIsOpen()) {
      try {
        ws.send(bytes);
        return true;
      } catch (e) {
        if (window.__hyouiDebug) window.__hyouiDebug('warn', 'ws.send failed: ' + e.message);
      }
    }
    // queue (上限あり)
    const size = typeof bytes === 'string' ? bytes.length : bytes.byteLength;
    if (wsPendingBytes + size <= WS_PENDING_MAX) {
      wsPendingInput.push(bytes);
      wsPendingBytes += size;
    }
    return false;
  }

  // xterm キー入力 → WS。onData は string (VT sequence 完成済み)、onBinary は
  // ISO-8859-1 として渡ってくる Uint8Array 相当 (mouse tracking 等)。
  term.onData((data) => {
    sendBytesToWs(data);
  });
  term.onBinary((data) => {
    // xterm.js の onBinary は Latin-1 string で bytes を渡してくる仕様。
    // そのまま send しても WS は text frame として送るので、Uint8Array に変換して
    // binary frame 化する (= daemon 側は bytes 透過で扱う)。
    const arr = new Uint8Array(data.length);
    for (let i = 0; i < data.length; i++) arr[i] = data.charCodeAt(i) & 0xff;
    sendBytesToWs(arr);
  });

  function connectWs() {
    if (wsExplicitClose) return;
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const url = `${proto}//${location.host}/api/sessions/${encodeURIComponent(sid)}/attach`;
    setWsStatus('connecting…');
    try {
      ws = new WebSocket(url);
    } catch (e) {
      setWsStatus('error: ' + e.message);
      scheduleWsReconnect();
      return;
    }
    ws.binaryType = 'arraybuffer';
    ws.onopen = () => {
      setWsStatus('connected');
      wsReconnectMs = 1000;
      // WS 接続中は screen ポーリング停止 (= 転送コスト削減 + reset で scrollback
      // が消えるのを avoid)。auto refresh checkbox は disable にして UI 上も
      // 「今は WS が主」であることを示す。
      if (timer) { clearInterval(timer); timer = null; }
      autoEl.disabled = true;
      autoEl.title = 'WS 接続中は auto refresh 無効 (scrollback / 選択保護)';
      flushPendingToWs();
      // 接続直後は daemon 側が redraw をまだ送っていない可能性がある。
      // 一度だけ screen dump を fetch して初期状態を xterm に流し込む
      // (= WS 経由の incremental だけだと画面が空のまま長時間になる懸念を潰す)。
      fetchScreen();
    };
    ws.onmessage = (ev) => {
      if (ev.data instanceof ArrayBuffer) {
        // Uint8Array を string 化して term.write。xterm.js は string でも
        // ArrayBuffer でも受けるが、既存 fetchScreen が string で書いているので
        // 統一しておく (= 差分が出にくい)。
        const u8 = new Uint8Array(ev.data);
        term.write(u8);
      } else if (typeof ev.data === 'string') {
        term.write(ev.data);
      }
    };
    ws.onclose = (ev) => {
      setWsStatus('disconnected (code=' + ev.code + ')');
      ws = null;
      // WS 切断後は fallback ポーリング + auto refresh 再有効化。
      autoEl.disabled = false;
      autoEl.title = '';
      if (wsExplicitClose) return;
      schedule();
      scheduleWsReconnect();
    };
    ws.onerror = (_e) => {
      setWsStatus('error');
      // onclose も後続で呼ばれる (= reconnect スケジューリングはそちらで)
    };
  }

  let wsReconnectTimer = null;
  function scheduleWsReconnect() {
    if (wsReconnectTimer) return;
    setWsStatus('reconnecting in ' + Math.round(wsReconnectMs / 1000) + 's…');
    wsReconnectTimer = setTimeout(() => {
      wsReconnectTimer = null;
      connectWs();
    }, wsReconnectMs);
    wsReconnectMs = Math.min(wsReconnectMs * 2, WS_RECONNECT_MAX_MS);
  }

  window.addEventListener('beforeunload', () => {
    wsExplicitClose = true;
    if (ws) try { ws.close(); } catch (_e) {}
  });

  autoEl.addEventListener('change', schedule);
  // refresh ボタン: WS 中に押すと reset が走って scrollback / 選択が消える。
  // 「明示押下は WS 中でも状態リセットの意図がある」ケース (= 画面が乱れた等) が
  // あり得るので nop にせず、確認ダイアログでガードする (誤タップ抑止)。
  refreshBtn.addEventListener('click', () => {
    if (wsIsOpen()) {
      if (!confirm('WS 接続中です。refresh すると scrollback と選択が消えます。実行しますか?')) return;
    }
    fetchScreen();
    refreshSessionStatus();
  });
  // PWA (standalone) 用の全ページリロード。詳細は index.js の該当箇所参照。
  const reloadBtn = document.getElementById('reload');
  if (reloadBtn) reloadBtn.addEventListener('click', () => location.reload());
  fetchScreen();
  refreshSessionStatus();
  schedule();
  // WS attach 開始 (= 成功すればポーリング停止、失敗しても指数バックオフで再試行)。
  connectWs();
  // status は screen より遅めに poll (= 一覧 API を頻繁に叩かない)。
  setInterval(refreshSessionStatus, 5000);
})();
