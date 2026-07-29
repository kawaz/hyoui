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
    // term.unicode は xterm.js の proposed API 扱いで、この option が無いと getter
    // 自体が throw する (= unicode11 addon の activate が register に到達できず、
    // 幅計算は UnicodeV6 のまま = 絵文字が幅 1 になる)。
    allowProposedApi: true,
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

  // ---- IME 変換位置の追従 (kawaz 実機フィードバック 2026-07-26) ----
  //
  // OS の IME 候補ウィンドウは「focus 中の editable 要素の *caret* 座標」に出る。
  // xterm.js は隠し textarea (.xterm-helper-textarea) を 1 セル幅でカーソル位置に
  // 移動させることでこれを実現しているが、v5.3.0 には追従が崩れる経路が 2 つある。
  // どちらも実機観測済み (playwright + CDP Input.imeSetComposition)。
  //
  // (A) textarea.value が確定文字列を溜め込み続ける
  //     CompositionHelper は compositionend 後も value をクリアしない。クリアするのは
  //     Enter / Ctrl-C の keydown 経路 (xterm.js `_keyDown`) だけだが、IME 確定の
  //     Enter は keyCode 229 として弾かれるためそこに到達しない。結果、同じ行で
  //     変換を重ねるほど value が伸び、幅 1 セルの textarea の中で content が右へ
  //     溢れる (実測: 5 語で scrollWidth 279px / clientWidth 8px = 271px の overflow)。
  //     caret は content 座標の右端に居るので、OS はセル位置ではなく溢れた先を見る。
  //     → 確定のたびに value を空へ戻し、caret を常に content 先頭へ張り付かせる。
  //
  // (B) resize 後に textarea が旧セル座標へ取り残される
  //     xterm.js は textarea を `onCursorMove` でしか同期しない (`_syncTextArea`)。
  //     resize でセル幅が変わってもカーソルの行列が変わらなければ move は発火せず、
  //     旧 metrics の座標が残る (実測: 1280→900px で 5.8px ズレ)。embed の iframe は
  //     ホスト側レイアウトで頻繁にリサイズされるのでこの経路を踏みやすい。
  //     → resize / 描画のたびに再同期する。
  //
  // 変換中 (isComposing) は CompositionHelper が textarea の幅・位置を変換文字列に
  // 合わせて拡張しているため、こちらからは触らない (= 上書きすると変換中の候補位置
  // を壊す)。xterm.js 本体の `_syncTextArea` も同じ理由で composing 中は早期 return
  // する。ここでは公開 API に無い内部にアクセスするので、存在チェック付きで呼ぶ
  // (= vendored xterm.js を差し替えた際に silently 壊れるより、追従を諦めて本来の
  // 挙動に戻るほうが安全)。
  //
  // Design rationale: 本来は upstream xterm.js を直すのが筋だが、vendored bundle は
  // minify 済みで、パッチを当てるとバージョン更新のたびに再適用が必要になる。
  // 外側から公開イベント (onRender / onResize) + 内部メソッド呼び出しで補正するほうが
  // vendor 差し替えに強い。
  const imeCore = term._core;
  const imeSupported = !!(imeCore && typeof imeCore._syncTextArea === 'function');
  if (!imeSupported && window.__hyouiDebug) {
    window.__hyouiDebug('warn', 'IME position sync unavailable (xterm internals changed)');
  }
  const imeIsComposing = () =>
    !!(imeCore && imeCore._compositionHelper && imeCore._compositionHelper.isComposing);
  const imeSync = () => {
    if (!imeSupported || imeIsComposing()) return;
    try { imeCore._syncTextArea(); } catch (_e) { /* vendor 差し替え時は諦める */ }
  };

  if (imeSupported) {
    const textarea = term.textarea;
    // (A) 確定した文字列を捨てる。compositionend の時点ではまだ CompositionHelper が
    // value を読んで daemon へ送る処理を setTimeout(0) で予約しているので、その後に
    // 走るよう同じく setTimeout(0) で遅延させる (= 先にクリアすると入力が消える)。
    if (textarea) {
      textarea.addEventListener('compositionend', () => {
        setTimeout(() => {
          if (imeIsComposing()) return;
          textarea.value = '';
          imeSync();
        }, 0);
      });
    }
    // (B) resize / 描画のたびに再同期。onRender は変換確定直後の再配置も拾う。
    term.onResize(imeSync);
    term.onRender(imeSync);
  }

  // ---- IME 変換キャンセル時の二重送信 (kawaz 実機フィードバック 2026-07-26) ----
  //
  // 再現: `mouhitotu,` と打って変換中 (画面は「もう一つ、」) に英数キーで変換を
  // 解除すると、「もう一つ、もう一つ、」と 2 回送られる。
  //
  // 原因は xterm.js v5.3.0 の `_finalizeComposition` が 2 回走ること。実測トレース:
  //
  //   英数 keydown  → CompositionHelper.keydown が「composing 中に 229 以外の
  //                   キーが来た」と判断し `_finalizeComposition(false)` を呼ぶ。
  //                   これは同期的に textarea の変換範囲を送る (送信 1 回目)。
  //                   同時に `_isComposing = false` になる。
  //   compositionend → ブラウザは composition の終了を必ず通知するので、その後
  //                   `_finalizeComposition(true)` が走る。`_isComposing` が既に
  //                   false なので `value.substring(start)` が同じ文字列のまま
  //                   残っており、もう一度送られる (送信 2 回目 = 重複)。
  //
  // 正常な Enter 確定では英数キー経路を通らないため `_finalizeComposition` は
  // compositionend 側の 1 回だけで、その時点の `_isComposing` は true。つまり
  // **compositionend 到達時に `_isComposing` が false なら、既に keydown 側で
  // 送信済み** という判別ができる (実測で両経路のフラグ値を確認済み)。
  //
  // 対処: compositionend を capture フェーズで先取りし、上記の「送信済み」条件に
  // 当てはまる場合だけ CompositionHelper へ伝播させない。xterm.js 側の
  // compositionend listener は bubble フェーズ登録なので、capture で
  // stopImmediatePropagation しても xterm の listener だけが止まり、
  // 我々の (A) のクリア処理は同じ capture フェーズで自前に呼べばよい。
  //
  // Design rationale: `_finalizeComposition` を直接差し替える手もあるが、minify
  // 済み内部関数の再実装は vendor 更新で壊れる。イベント経路で止めるほうが、
  // 内部実装が変わっても「重複を防げなくなる」だけで済み、入力欠落にはならない。
  if (imeSupported && term.textarea) {
    const textarea = term.textarea;
    textarea.addEventListener('compositionend', (ev) => {
      const helper = imeCore._compositionHelper;
      // `_isComposing` が false = keydown 経路で finalize 済み (= 送信済み)。
      // このまま xterm.js に渡すと同じ文字列がもう一度送られる。
      if (helper && helper.isComposing === false) {
        ev.stopImmediatePropagation();
        // xterm.js の listener を止めた分、value のクリアと位置同期は自前で行う
        // (= (A) の後始末が走らなくなるため)。
        setTimeout(() => {
          textarea.value = '';
          imeSync();
        }, 0);
        if (window.__hyouiDebug) {
          window.__hyouiDebug('info', 'IME cancel: suppressed duplicate composition send');
        }
      }
    }, true);
  }

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

  // 位置保存: sessionStorage にエッジ相対で保存 (kawaz r40m97 / r40m98)。
  //
  // - 保存キーは sessionStorage の 1 本に統合。**リロードで default (右下) に戻る**
  //   のは「見失った時のリカバリ手段」として意図的 (localStorage で永続化しない)。
  // - 位置は絶対 x,y ではなく **エッジ相対** に保存:
  //   `{ hEdge: 'left'|'right', hDist, vEdge: 'top'|'bottom', vDist }`。
  //   基準辺は「要素の中心が viewport 中心のどちら側か」で決める (中央同点は
  //   右 / 下を優先、default 位置と整合)。回転 / resize でも「基準辺からの距離」
  //   で再配置されるため、画面中央や画面外に浮いた状態が構造的に発生しない。
  // - 旧 localStorage キー (Phase B / Phase C の {x,y}) は残っていたら掃除する。
  const SS_POS = 'hyoui.session.floatPos';
  const LS_POS_LEGACY_KEYS = ['hyoui.session.floatPos', 'hyoui.session.fabPos'];

  let floatPos = null; // { hEdge, hDist, vEdge, vDist } or null (= default 右下)

  function loadPos() {
    try {
      const raw = sessionStorage.getItem(SS_POS);
      if (!raw) return null;
      const p = JSON.parse(raw);
      if (
        (p?.hEdge === 'left' || p?.hEdge === 'right') &&
        (p?.vEdge === 'top' || p?.vEdge === 'bottom') &&
        typeof p.hDist === 'number' &&
        typeof p.vDist === 'number'
      ) return p;
    } catch (_e) { /* private mode 等 */ }
    return null;
  }
  function savePos(p) {
    floatPos = p;
    try { sessionStorage.setItem(SS_POS, JSON.stringify(p)); } catch (_e) {}
  }
  // 旧 localStorage の絶対座標保存は掃除 (1 度だけ)。
  try { LS_POS_LEGACY_KEYS.forEach((k) => localStorage.removeItem(k)); } catch (_e) {}

  // 要素の rect と viewport から edge-relative pos を導出。
  // 中心が viewport の右半 (中央同点含む) にあれば hEdge='right'、それ以外は 'left'。
  // 下半 (同点含む) なら vEdge='bottom'、それ以外は 'top'。
  function rectToEdgePos(rect) {
    const cx = rect.left + rect.width / 2;
    const cy = rect.top + rect.height / 2;
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    const hEdge = cx >= vw / 2 ? 'right' : 'left';
    const vEdge = cy >= vh / 2 ? 'bottom' : 'top';
    const hDist = hEdge === 'right' ? Math.max(0, vw - rect.right) : Math.max(0, rect.left);
    const vDist = vEdge === 'bottom' ? Math.max(0, vh - rect.bottom) : Math.max(0, rect.top);
    return { hEdge, hDist, vEdge, vDist };
  }
  // pos (edge-relative) を el に適用。el の実寸で viewport clamp してから絶対座標へ変換。
  function applyEdgePos(el, pos) {
    const r = el.getBoundingClientRect();
    const w = r.width || 51;
    const h = r.height || 51;
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    // 基準辺から目標距離。反対辺までの余白があるかを確認して clamp。
    // 反対辺までの最小余白は 0 (画面内に必ず収める)。
    const maxH = Math.max(0, vw - w);
    const maxV = Math.max(0, vh - h);
    const hDist = Math.max(0, Math.min(maxH, pos.hDist));
    const vDist = Math.max(0, Math.min(maxV, pos.vDist));
    const x = pos.hEdge === 'right' ? vw - w - hDist : hDist;
    const y = pos.vEdge === 'bottom' ? vh - h - vDist : vDist;
    el.style.left = x + 'px';
    el.style.top = y + 'px';
    el.style.right = 'auto';
    el.style.bottom = 'auto';
  }
  // drag / open / close で「今 el がここにある」を新 pos として保存 + 反映する。
  function applyAndSaveFromRect(el, x, y) {
    // 一旦仮 rect を作って edge pos を計算 (要素の現在サイズ基準)
    const r = el.getBoundingClientRect();
    const fakeRect = { left: x, top: y, right: x + r.width, bottom: y + r.height, width: r.width, height: r.height };
    const pos = rectToEdgePos(fakeRect);
    applyEdgePos(el, pos);
    savePos(pos);
    return pos;
  }
  // Query パラメータからの初期位置 + デザインヒント指定
  // (kawaz r40m99: ccmsg 側の＋ボタン被り対策 / r56: ccmsg UI と馴染ませる用の size/bg/fg 追加)。
  // 優先順位: sessionStorage (pos のみ) > query > default (CSS の右下 1rem、CSS の色/サイズ)。
  //
  // 受理形式 (どちらでも / 両方併用時は個別 param が単一 param を上書き):
  //   1) 単一 param:   ?fab=<key>:<val>[,<key>:<val>...]
  //        位置キー:  left/right/top/bottom  (値: 非負数、px)
  //        デザインキー: size (32〜96 に clamp), bg (CSS color), fg (CSS color)
  //        例: ?fab=right:16,bottom:64,size:48,bg:%233b82f6,fg:white
  //   2) 個別 param:   ?fab-<key>=<val>
  //        例: ?fab-right=16&fab-bottom=64&fab-size=48&fab-bg=%233b82f6
  //
  // ルール:
  //   - edge は right/left (水平) と top/bottom (垂直) から片方ずつ選ぶ
  //   - 位置 dist / size は非負数値、size は 32〜96 に clamp
  //   - bg/fg は CSS.supports('color', v) で検証、不正は silent skip
  //   - 未知 key は silent skip (前方互換)
  //   - 両側指定は「後勝ち」= 個別 param → 単一 param 内は右辺後勝ち
  //   - 片側 (h/v) 未指定なら default (右 or 下 16px)
  const FAB_SIZE_MIN = 32, FAB_SIZE_MAX = 96;
  const FAB_FONT_RATIO = 2.1 / 3.2; // CSS default 51.2px 径 : 33.6px font の比を維持
  function parseFabFromQuery() {
    const q = new URLSearchParams(location.search);
    let hEdge = null, hDist = null, vEdge = null, vDist = null;
    const design = { size: null, bg: null, fg: null };
    const assign = (key, raw) => {
      const k = key.toLowerCase();
      if (k === 'left' || k === 'right' || k === 'top' || k === 'bottom') {
        const d = parseFloat(raw);
        if (!Number.isFinite(d) || d < 0) return;
        if (k === 'left' || k === 'right') { hEdge = k; hDist = d; }
        else { vEdge = k; vDist = d; }
      } else if (k === 'size') {
        const n = parseFloat(raw);
        if (!Number.isFinite(n)) return;
        design.size = Math.max(FAB_SIZE_MIN, Math.min(FAB_SIZE_MAX, n));
      } else if (k === 'bg' || k === 'fg') {
        // CSS.supports で検証 (injection 対策: 検証通過値を style プロパティへ直代入のみ)
        if (typeof CSS !== 'undefined' && CSS.supports && CSS.supports('color', raw)) {
          design[k] = raw;
        }
      }
      // 未知 key は silent skip
    };
    const single = q.get('fab');
    if (single) {
      for (const part of single.split(',')) {
        const s = part.trim();
        const idx = s.indexOf(':');
        if (idx < 0) continue;
        assign(s.slice(0, idx).trim(), s.slice(idx + 1).trim());
      }
    }
    for (const key of ['left', 'right', 'top', 'bottom', 'size', 'bg', 'fg']) {
      const v = q.get('fab-' + key);
      if (v == null) continue;
      assign(key, v);
    }
    let pos = null;
    if (hEdge !== null || vEdge !== null) {
      if (hEdge === null) { hEdge = 'right'; hDist = 16; }
      if (vEdge === null) { vEdge = 'bottom'; vDist = 16; }
      pos = { hEdge, hDist, vEdge, vDist };
    }
    const hasDesign = design.size !== null || design.bg !== null || design.fg !== null;
    return { pos, design: hasDesign ? design : null };
  }
  function applyFabDesign(el, design) {
    if (!design) return;
    if (design.size !== null) {
      el.style.width = design.size + 'px';
      el.style.height = design.size + 'px';
      el.style.fontSize = (design.size * FAB_FONT_RATIO) + 'px';
    }
    if (design.bg !== null) {
      // bg を CSS custom property 経由でセットし、border / hover 派生色を
      // color-mix(in oklch, ...) で CSS 側に生成させる (kawaz r56、ccmsg UI 馴染ませ用)。
      // design.bg は CSS.supports('color', ...) 検証通過済み → color-mix の引数として
      // 直挿しても injection safe (custom property 値は宣言外へ抜けない)。
      // 数値 70%/85% は「bg より一段明るい border」「bg のわずかに明るい hover」を意図。
      el.style.setProperty('--fab-bg', design.bg);
      el.style.setProperty('--fab-border', `color-mix(in oklch, ${design.bg} 70%, white)`);
      el.style.setProperty('--fab-hover-bg', `color-mix(in oklch, ${design.bg} 85%, white)`);
    }
    if (design.fg !== null) el.style.color = design.fg;
  }

  // 初期位置 + デザインヒント restore: sessionStorage (pos) > query > CSS default (右下 1rem)。
  floatPos = loadPos();
  const fromQuery = parseFabFromQuery();
  const queryPos = floatPos ? null : fromQuery.pos;
  // デザインヒントは sessionStorage に保存しない (= query 経由専用、reload で再解釈)。
  // size 反映後に位置計算が正しくなるよう、design → pos の順で適用。
  applyFabDesign(inputFab, fromQuery.design);
  const initialPos = floatPos ?? queryPos;
  if (initialPos) {
    // query 経由 pos は sessionStorage に書かない (= reload / タブ再利用で query を再解釈させる)。
    requestAnimationFrame(() => applyEdgePos(inputFab, initialPos));
    if (queryPos) floatPos = queryPos; // resize/drag の基準として持つ
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
        applyAndSaveFromRect(mover, ev.clientX - state.offsetX, ev.clientY - state.offsetY);
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
    // FAB と Panel は「同一浮遊物の折りたたみ/展開」なので、位置引き継ぎは
    // 左上座標でなく edge-relative pos (floatPos) を共有する。左上座標で
    // 引き継ぐと、幅の違う要素間でアンカー辺の判定 (中心点) が反転し、
    // 「右端で開いて閉じたら左端に飛ぶ」バグになる (kawaz 実機 2026-07-23)。
    const pos = floatPos || rectToEdgePos(inputFab.getBoundingClientRect());
    inputFab.hidden = true;
    inputPanel.hidden = false;
    applyEdgePos(inputPanel, pos);
    savePos(pos);
    requestAnimationFrame(() => {
      inputText.focus();
      const l = inputText.value.length;
      try { inputText.setSelectionRange(l, l); } catch (_e) {}
    });
  }
  function closePanel() {
    // openPanel と対称: edge-relative pos を共有して FAB に適用する
    // (Panel を drag した場合は drag handler が floatPos を更新済み)。
    const pos = floatPos || rectToEdgePos(inputPanel.getBoundingClientRect());
    inputPanel.hidden = true;
    inputFab.hidden = false;
    applyEdgePos(inputFab, pos);
    savePos(pos);
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
    // Keys 開閉で height が変わるので、edge-relative 保存位置ベースで再配置。
    // (絶対座標に落として applyAndSaveFromRect でなく、既に持っている edge pos
    //  を再適用 = 「開いたら bottom 基準から下辺に貼り付く」動きが自然)。
    requestAnimationFrame(() => {
      if (floatPos) applyEdgePos(inputPanel, floatPos);
    });
  });

  // viewport resize (回転 / iframe サイズ変化) 時に visible 側を edge-relative で
  // 再配置。edge 基準の距離で貼り付いているので、画面中央や画面外に浮くことなく
  // 「右下から 16px」等の相対位置が保たれる。
  window.addEventListener('resize', () => {
    if (!floatPos) return;
    const target = inputPanel.hidden ? inputFab : inputPanel;
    applyEdgePos(target, floatPos);
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

  // ---- Shift+Enter (kawaz 実機フィードバック 2026-07-26) ----
  //
  // xterm.js v5.3.0 の keymap は Enter で shiftKey を見ない
  // (`case 13: o.key = e.altKey ? ESC+CR : CR`) ため、Shift+Enter でも素の CR
  // が飛び、claude TUI ではメッセージが送信されてしまう。期待は「送信せず改行」。
  //
  // 何を送るのが正かは実測で確定させた。claude TUI (v2.1.220) を hyoui 配下で
  // 起動して起動時の出力 bytes を観測すると、以下を有効化している:
  //
  //   ESC [ < u        kitty keyboard protocol を pop (継承状態のリセット)
  //   ESC [ > 1 u      kitty keyboard protocol を push (flags=1 = disambiguate)
  //   ESC [ > 4 ; 2 m  xterm modifyOtherKeys level 2
  //
  // その上で実際に bytes を送り込んで prompt の挙動を確認した (実測マトリクス):
  //
  //   0x0d (CR)          → メッセージ送信 (= 素の Enter)
  //   0x0a (LF)          → 送信せず改行
  //   ESC [ 1 3 ; 2 u    → 送信せず改行 (= kitty CSI-u の Shift+Enter)
  //
  // LF でも通るが、claude が kitty protocol を明示的に有効化している以上、
  // Shift+Enter を表す正規の表現である CSI-u を送る。CSI-u なら「Shift 修飾付き
  // Enter」という情報が保たれるので、修飾キーを区別するアプリに対しても正しい
  // (LF は「改行文字」であって Shift+Enter ではない)。
  //
  // Design rationale: `attachCustomKeyEventHandler` は true/false しか返せず
  // 送信 bytes を差し替えられないため、false を返して xterm.js の既定処理
  // (CR 送信) を止めた上で、自前で CSI-u を WS へ流す。keydown だけを捕まえ、
  // keypress / keyup は xterm.js 側で自然に無視される。
  const SHIFT_ENTER_CSI_U = '\x1b[13;2u';
  term.attachCustomKeyEventHandler((ev) => {
    if (ev.type !== 'keydown') return true;
    // 修飾は Shift のみ。Ctrl/Alt/Meta 併用は別のキーバインドなので触らない
    // (= xterm.js の既定処理に委ねる)。
    if (ev.key !== 'Enter' || !ev.shiftKey || ev.ctrlKey || ev.altKey || ev.metaKey) return true;
    ev.preventDefault();
    sendBytesToWs(SHIFT_ENTER_CSI_U);
    return false; // xterm.js に CR を送らせない
  });

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
