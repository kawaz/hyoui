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
  const inputText = document.getElementById('inputText');
  const sendBtn = document.getElementById('sendBtn');
  const sendKeyBtn = document.getElementById('sendKey');
  const sendStatus = document.getElementById('sendStatus');
  const sendEnterAfter = document.getElementById('sendEnterAfter');
  const inputFab = document.getElementById('inputFab');
  const inputPanel = document.getElementById('inputPanel');
  const inputPanelClose = document.getElementById('inputPanelClose');
  const keypadToggle = document.getElementById('keypadToggle');
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

  // multi-line 対応の送信ロジック (kawaz 要望 2026-07-23)。
  // - textarea の中身を \n で split
  // - 各行を text:<line> として送信
  // - sendEnterAfter が checked (default) なら各行末に key:Enter を挟む
  //   (= shell では 0x0d が Enter、0x0a を text で送っても cooked mode で扱いが
  //    OS 依存になるため、key:Enter で 0x0d を明示的に送る)
  // - 送信後は textarea を空にする (再送信リスク回避)
  function submitFromTextarea() {
    const raw = inputText.value;
    if (raw.length === 0 && !sendEnterAfter.checked) return;
    const lines = raw.split('\n');
    const specs = [];
    lines.forEach((line, idx) => {
      if (line.length > 0) specs.push('text:' + line);
      if (sendEnterAfter.checked) {
        specs.push('key:Enter');
      } else if (idx < lines.length - 1) {
        // Enter/行 off でも改行区切りは残す (= hex で 0x0a 送信)
        specs.push('hex:0a');
      }
    });
    if (specs.length === 0) return;
    sendSpecs(specs);
    inputText.value = '';
  }

  sendBtn.addEventListener('click', submitFromTextarea);

  // textarea key handling: Cmd+Enter / Ctrl+Enter で送信、Enter 単体は改行 (default 動作)。
  // IME 変換確定の Enter を誤送信しない (= isComposing / keyCode 229 の除外)。
  inputText.addEventListener('keydown', (ev) => {
    if (ev.key === 'Enter' && (ev.metaKey || ev.ctrlKey)) {
      // 変換中の Enter は Cmd/Ctrl 付きでは通常来ないが念のため除外
      if (ev.isComposing || ev.keyCode === 229) return;
      ev.preventDefault();
      submitFromTextarea();
    }
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

  // ---- Floating input: FAB ⇔ Panel は同一の浮遊物 (kawaz 実機 FB 2026-07-23) ----
  //
  // メンタルモデル: FAB と Panel は「折り畳み状態 / 展開状態」の 2 モードを持つ
  // 1 つの浮遊物。Panel は FAB の位置に「置き換え」で出て、閉じると **その時の
  // Panel 位置** に FAB が戻る (位置は連動、localStorage は 1 つ)。
  //
  // - FAB / Panel いずれもドラッグで移動可 (touch 対応)。Panel はヘッダを掴む
  // - viewport clamp: 表示中の要素の実寸で clamp
  // - Cmd/Ctrl+Enter で送信、Enter は改行 (上の keydown ハンドラ参照)
  // - キーパッドは折り畳み式 (default: 畳んだ状態)
  // - WS attach との協調: パネル展開中は textarea にフォーカス、閉じたら xterm
  //   ヘルパー textarea にフォーカスを戻して直打ちを継続

  const LS_POS = 'hyoui.session.floatPos'; // 統合された位置 (旧 LS_FAB_POS は back-compat で吸収)
  const LS_POS_LEGACY = 'hyoui.session.fabPos';

  // 位置ステート: null なら default (CSS の右下)。set したら FAB / Panel どちら
  // にでも共通で適用する。localStorage には常にこの値を書く。
  let floatPos = null;

  function loadPos() {
    try {
      const raw = localStorage.getItem(LS_POS) ?? localStorage.getItem(LS_POS_LEGACY);
      if (!raw) return null;
      const p = JSON.parse(raw);
      if (typeof p?.x === 'number' && typeof p?.y === 'number') return { x: p.x, y: p.y };
    } catch (_e) { /* private mode 等 */ }
    return null;
  }
  function savePos(p) {
    floatPos = p;
    try {
      localStorage.setItem(LS_POS, JSON.stringify(p));
      // legacy key は捨てる (1 位置に統合)
      localStorage.removeItem(LS_POS_LEGACY);
    } catch (_e) {}
  }
  function clampToViewport(x, y, el) {
    const r = el.getBoundingClientRect();
    const w = r.width || 51;
    const h = r.height || 51;
    const maxX = Math.max(0, window.innerWidth - w);
    const maxY = Math.max(0, window.innerHeight - h);
    return { x: Math.max(0, Math.min(maxX, x)), y: Math.max(0, Math.min(maxY, y)) };
  }
  function applyPos(el, x, y) {
    const p = clampToViewport(x, y, el);
    el.style.left = p.x + 'px';
    el.style.top = p.y + 'px';
    el.style.right = 'auto';
    el.style.bottom = 'auto';
    return p;
  }
  // 初期位置 restore (default は CSS の右下、bottom/right が効いた状態)。
  floatPos = loadPos();
  if (floatPos) {
    requestAnimationFrame(() => { applyPos(inputFab, floatPos.x, floatPos.y); });
  }

  // 共通の pointer drag。target = 掴む要素 (FAB or panel header)、
  // move = 実際に位置を変える要素 (FAB or panel)。tap 時に click を発火させる
  // かどうかは caller (onTap 経由) が決める。
  const DRAG_THRESHOLD_PX = 5;
  function attachDrag(target, mover, opts) {
    let state = null;
    let suppressClick = false;
    target.addEventListener('pointerdown', (ev) => {
      if (ev.button !== undefined && ev.button !== 0) return;
      // 掴む対象外 (close ボタン等) は無視
      if (opts?.ignoreTarget?.(ev.target)) return;
      const r = mover.getBoundingClientRect();
      state = {
        startX: ev.clientX,
        startY: ev.clientY,
        offsetX: ev.clientX - r.left,
        offsetY: ev.clientY - r.top,
        moved: false,
        pointerId: ev.pointerId,
      };
      try { target.setPointerCapture(ev.pointerId); } catch (_e) {}
    });
    target.addEventListener('pointermove', (ev) => {
      if (!state || ev.pointerId !== state.pointerId) return;
      const dx = ev.clientX - state.startX;
      const dy = ev.clientY - state.startY;
      if (!state.moved && Math.hypot(dx, dy) > DRAG_THRESHOLD_PX) {
        state.moved = true;
        mover.classList.add('dragging');
      }
      if (state.moved) {
        const p = applyPos(mover, ev.clientX - state.offsetX, ev.clientY - state.offsetY);
        savePos(p);
      }
    });
    target.addEventListener('pointerup', (ev) => {
      if (!state || ev.pointerId !== state.pointerId) return;
      const moved = state.moved;
      mover.classList.remove('dragging');
      try { target.releasePointerCapture(state.pointerId); } catch (_e) {}
      if (moved) suppressClick = true;
      else if (opts?.onTap) opts.onTap();
      state = null;
    });
    target.addEventListener('pointercancel', (ev) => {
      if (state && ev.pointerId === state.pointerId) {
        mover.classList.remove('dragging');
        state = null;
      }
    });
    // drag 直後の native click を吸収 (pointer 経由の tap は onTap で処理済み)
    target.addEventListener('click', (ev) => {
      if (suppressClick) {
        suppressClick = false;
        ev.stopPropagation();
        ev.preventDefault();
        return;
      }
      // keyboard 経由の click (detail=0) は FAB の場合のみ toggle 扱い (accessibility)
      if (ev.detail === 0 && opts?.onTap) opts.onTap();
    });
  }

  function openPanel() {
    // FAB の現在位置 (rect) をそのまま Panel の左上に引き継ぎ、FAB を隠す。
    // savePos は Panel 側でも同じ位置を使うので統一される。
    const r = inputFab.getBoundingClientRect();
    inputFab.hidden = true;
    inputPanel.hidden = false;
    // 表示直後は width/height が確定するので、それを見て clamp
    const p = applyPos(inputPanel, r.left, r.top);
    savePos(p);
    requestAnimationFrame(() => {
      inputText.focus();
      const l = inputText.value.length;
      try { inputText.setSelectionRange(l, l); } catch (_e) {}
    });
  }
  function closePanel() {
    // Panel の現在位置 (rect) を FAB の左上に引き継ぐ (= 「位置引き継ぎ」)。
    const r = inputPanel.getBoundingClientRect();
    inputPanel.hidden = true;
    inputFab.hidden = false;
    const p = applyPos(inputFab, r.left, r.top);
    savePos(p);
    try {
      const helper = document.querySelector('.xterm-helper-textarea');
      if (helper) helper.focus();
    } catch (_e) {}
  }
  function togglePanel() {
    if (inputPanel.hidden) openPanel();
    else closePanel();
  }

  // FAB: 全体で drag、tap で open。
  attachDrag(inputFab, inputFab, { onTap: togglePanel });

  // Panel: ヘッダ (× ボタン除く) で drag。tap は open/close トグルではない
  // (= ヘッダを軽くタップしても閉じないほうが安全)。
  const panelHead = inputPanel.querySelector('.input-panel-head');
  attachDrag(panelHead, inputPanel, {
    ignoreTarget: (t) => t.closest('.input-panel-close'),
  });

  inputPanelClose.addEventListener('click', closePanel);
  // Esc で閉じても textarea 内容は残す (誤クローズ時の入力保護)。
  // ただしパネル外クリックでの誤クローズはさせない (kawaz 明示)。
  inputPanel.addEventListener('keydown', (ev) => {
    if (ev.key === 'Escape') {
      ev.preventDefault();
      closePanel();
    }
  });

  // Keys 折り畳み。default 畳み。開閉で高さが変わるので clamp を掛け直す。
  const keypadEl = document.getElementById('keypad');
  keypadToggle.addEventListener('click', () => {
    const willShow = keypadEl.hidden;
    keypadEl.hidden = !willShow;
    keypadToggle.setAttribute('aria-expanded', willShow ? 'true' : 'false');
    keypadToggle.textContent = willShow ? 'Keys ▴' : 'Keys ▾';
    requestAnimationFrame(() => {
      const r = inputPanel.getBoundingClientRect();
      const p = applyPos(inputPanel, r.left, r.top);
      savePos(p);
    });
  });

  // viewport resize (回転 / iframe サイズ変化) 時に visible 側を clamp。
  window.addEventListener('resize', () => {
    const target = inputPanel.hidden ? inputFab : inputPanel;
    const r = target.getBoundingClientRect();
    const p = applyPos(target, r.left, r.top);
    savePos(p);
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
