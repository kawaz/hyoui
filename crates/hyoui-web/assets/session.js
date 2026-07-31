// hyoui web — session page.
// Fetches /api/sessions/:id/screen (ANSI) every few seconds, writes bytes into
// an xterm.js instance. Input form POSTs to /api/sessions/:id/input.
(async () => {
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

  // ---- 表示設定の URL query 上書き (kawaz 要望 2026-07-29) ----
  //
  // iframe 埋め込み側 (ccmsg webui Terminal タブ等) がホスト UI に合わせて
  // フォント・色・scrollback を選べるよう、xterm.js の表示オプションを query で
  // 上書きできるようにする。優先は「query → 既定」の 1 段だけで、config ファイル
  // ([web]) との連携はしない (= 表示は閲覧するクライアント側の都合であって
  // daemon/gateway の設定ではない)。
  //
  // 不正値は既定に落として warn を出す。埋め込み先では devtools が見えないことが
  // あるので console.warn 経由で debug panel (session.html の __hyouiDebug) にも
  // 流れるようにしている。
  //
  //   fontsize=<6..40>      px。既定 13
  //   lineheight=<1.0..2.0> 既定 1.0 (xterm.js は 1 未満を throw で弾く)
  //   scrollback=<0..100000> 行。既定 2000
  //   fontfamily=<names>    カンマ区切り。既定チェーンの先頭に挿入する
  //   bg=<hex> / fg=<hex>   背景 / 前景。`#` は省略可 (URL で %23 を書かずに済む)
  //   unicode=<6|11>        文字幅計算の Unicode 版。既定 11
  //   ambw=<half|full>      East Asian Ambiguous の幅。既定 half
  const params = new URLSearchParams(location.search);
  const displaySettingSources = {};
  const settingValue = (name, value, source) => {
    displaySettingSources[name] = source;
    return value;
  };
  const warnParam = (name, raw, why) => {
    console.warn(`hyoui: ignoring ?${name}=${raw} (${why})`);
  };
  const numParam = (name, def, min, max, integer) => {
    const raw = params.get(name);
    if (raw === null) return settingValue(name, def, 'default');
    const v = Number(raw.trim());
    if (raw.trim() === '' || !Number.isFinite(v)) {
      warnParam(name, raw, 'not a number');
      return settingValue(name, def, 'default');
    }
    if (v < min || v > max) {
      warnParam(name, raw, `out of range ${min}-${max}`);
      return settingValue(name, def, 'default');
    }
    return settingValue(name, integer ? Math.round(v) : v, 'url');
  };
  const enumParam = (name, def, allowed) => {
    const raw = params.get(name);
    if (raw === null) return settingValue(name, def, 'default');
    const s = raw.trim();
    if (!allowed.includes(s)) {
      warnParam(name, raw, `expected one of ${allowed.join('|')}`);
      return settingValue(name, def, 'default');
    }
    return settingValue(name, s, 'url');
  };
  const normalizeColor = (raw) => {
    const s = raw.trim().replace(/^#/, '');
    // 3/4/6/8 桁 hex (xterm.js の theme は #RGB / #RRGGBB / #RRGGBBAA を解釈する)
    if (!/^(?:[0-9a-fA-F]{3,4}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})$/.test(s)) {
      throw new Error('not a hex color');
    }
    return '#' + s;
  };
  const colorParam = (name, def) => {
    const raw = params.get(name);
    if (raw === null) return settingValue(name, def, 'default');
    try {
      return settingValue(name, normalizeColor(raw), 'url');
    } catch (e) {
      warnParam(name, raw, e.message);
      return settingValue(name, def, 'default');
    }
  };
  // 半角ブロック罫線 (▀▄▉ 等) の上下ズレ対策として lineHeight は 1.0 が既定。
  // fontFamily は同梱の HackGen Console NF (SIL OFL、半角:全角=1:2、Nerd Font +
  // 日本語 JIS 第 1-2 水準入り) を最優先。host 依存フォントを消してメトリクス
  // を安定化させる。fallback は既存 monospace 列。
  const DEFAULT_FONT_FAMILY =
    '"HackGen Console NF", Menlo, "DejaVu Sans Mono", Consolas, "Courier New", monospace';
  const normalizeFontFamily = (raw) => {
    const s = raw.trim();
    // 値は xterm.js が element の inline style に直接入れるため、font-family 名に
    // 現れうる文字だけを通す (= `;` `{` `(` 等を弾いて他プロパティの注入を防ぐ)。
    if (!s || !/^[\w \-'",.]+$/.test(s)) throw new Error('invalid font family name');
    if (s === DEFAULT_FONT_FAMILY || s.endsWith(`, ${DEFAULT_FONT_FAMILY}`)) return s;
    const list = s.split(',').map((t) => t.trim().replace(/['"]/g, '')).filter(Boolean);
    if (list.length === 0) throw new Error('empty font family name');
    const quoted = list.map((t) => (/^[\w-]+$/.test(t) ? t : `"${t}"`));
    return quoted.concat(DEFAULT_FONT_FAMILY).join(', ');
  };
  const fontFamilyParam = () => {
    const raw = params.get('fontfamily');
    if (raw === null) return settingValue('fontfamily', DEFAULT_FONT_FAMILY, 'default');
    try {
      return settingValue('fontfamily', normalizeFontFamily(raw), 'url');
    } catch (e) {
      warnParam('fontfamily', raw, e.message);
      return settingValue('fontfamily', DEFAULT_FONT_FAMILY, 'default');
    }
  };

  let fontSize = numParam('fontsize', 13, 6, 40, true);
  let lineHeight = numParam('lineheight', 1.0, 1.0, 2.0, false);
  let scrollback = numParam('scrollback', 2000, 0, 100000, true);
  let fontFamily = fontFamilyParam();
  let background = colorParam('bg', '#111');
  let foreground = colorParam('fg', '#e0e0e0');
  // unicode: 11 が既定。6 は xterm.js 内蔵の UnicodeV6 テーブルで、Unicode 6 当時に
  // 存在しなかった絵文字が幅 1 になる (= v0.9.25 より前の hyoui web の挙動)。
  let unicodeVersion = enumParam('unicode', '11', ['6', '11']);
  let ambWidth = enumParam('ambw', 'half', ['half', 'full']);
  // #term は style.css で背景 #111 固定。padding 4px 分がターミナル本体の外側に
  // 覗くので、bg を変えたらコンテナ側も合わせる。
  const termEl = document.getElementById('term');
  if (termEl) termEl.style.background = background;

  const term = new Terminal({
    cols: COLS,
    rows: ROWS,
    convertEol: false,
    scrollback,
    // DR-0027 Phase 3: WS attach 接続後は raw stream で xterm.js が直接キー入力を
    // WS に流すため、stdin を有効化する。WS 未接続時のフォーカス入力は WS が
    // 復活するまで無害 (= term.onData で input queue に貯まるが送り先が無いので
    // WS 接続時に flush される。UX 上は input 欄経由が primary な fallback)。
    disableStdin: false,
    // term.unicode は xterm.js の proposed API 扱いで、この option が無いと getter
    // 自体が throw する (= unicode11 addon の activate が register に到達できず、
    // 幅計算は UnicodeV6 のまま = 絵文字が幅 1 になる)。
    allowProposedApi: true,
    lineHeight,
    fontFamily,
    fontSize,
    theme: { background, foreground },
  });
  // 文字幅 provider は初期 URL 設定と情報パネル内の runtime 変更で同じ関数を使う。
  // ambw provider は Unicode 版ごとに 1 回だけ register し、以後は activeVersion の
  // 切替だけにする (= 同じ version の重複登録や provider の無制限増加を避ける)。
  const registeredAmbwVersions = new Set();
  let unicodeWidthAvailable = false;
  function applyUnicodeWidth() {
    if (!unicodeWidthAvailable) throw new Error('Unicode width provider is unavailable');
    term.unicode.activeVersion = unicodeVersion;
    if (ambWidth === 'full') {
      const version = `${unicodeVersion}-ambw`;
      if (!registeredAmbwVersions.has(version)) {
        // V11 は addon の公開 activate 経路で取得できる。V6 は constructor 組み込みで
        // addon が無いため、固定 vendor の provider table を存在確認付きで参照する。
        const base = unicodeVersion === '11'
          ? window.HyouiAmbiguousWidth.captureUnicode11()
          : (term._core.unicodeService._providers || {})[unicodeVersion];
        if (!base) throw new Error(`no width provider for unicode=${unicodeVersion}`);
        term.unicode.register(window.HyouiAmbiguousWidth.wrapProvider(base, version));
        registeredAmbwVersions.add(version);
      }
      term.unicode.activeVersion = version;
    }
    if (window.__hyouiDebug) {
      window.__hyouiDebug('info', 'unicode activeVersion=' + term.unicode.activeVersion);
    }
  }
  if (window.Unicode11Addon) {
    try {
      term.loadAddon(new window.Unicode11Addon.Unicode11Addon());
      unicodeWidthAvailable = true;
      applyUnicodeWidth();
    } catch (e) {
      if (window.__hyouiDebug) window.__hyouiDebug('warn', 'unicode width setup failed: ' + e.message);
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
  // ---- webfont のロードを待ってから open する (font-load / fit race 対策) ----
  //
  // xterm.js は open() 時に測った 1 セルの実寸で grid を確定し、以降は要素の実寸が
  // 変わるまで再測定しない。fit() / refresh() / 同値の fontFamily 再設定でも
  // 再測定されないことを実測済み (docs/issue/2026-07-30-bug-web-terminal-font-load-
  // fit-race.md)。webfont 未ロードのまま open すると fallback フォントのセル寸法
  // (実測 7.81px) で cols/rows が固まり、HackGen の実寸 (6.84px) に切り替わるのは
  // 次に viewport が動いた時になる。?resize=1 ではその誤ったサイズが PTY へ送られ、
  // 初回表示直後に子 TUI が不自然に再レイアウトされる。
  //
  // style.css の font-display: block は glyph の描画を遅らせるだけで、JS からの
  // セル測定も open() も block しないため、ここで明示的に待つ必要がある。
  // 待つのは実際に使う font shorthand そのもの (= query の fontfamily を含む
  // チェーン全体) で、既定の HackGen だけを特別扱いしない。
  //
  // フォントを配れない環境 (404 / オフライン / 未対応ブラウザ) で terminal 自体が
  // 起動しなくなるのは避けたいので、reject も timeout も「fallback フォントの
  // メトリクスで続行」として扱う。
  const FONT_LOAD_TIMEOUT_MS = 2000;
  if (document.fonts && typeof document.fonts.load === 'function') {
    try {
      await Promise.race([
        document.fonts.load(`${fontSize}px ${fontFamily}`),
        new Promise((resolve) => setTimeout(resolve, FONT_LOAD_TIMEOUT_MS)),
      ]);
    } catch (e) {
      console.warn(`hyoui: font load failed (${e.message}), using fallback metrics`);
    }
  }
  term.open(termEl);
  // font load が timeout した場合の保険。open 後にセル寸法が変わったら grid を
  // 作り直す。xterm.js 5.3 の fit() は自前で再測定しないので、内部の
  // CharSizeService に測り直させてから fit する (= 公開 API に代替が無い。
  // dispose → 再 open は scrollback と WS の状態を失うので採らない)。
  // 内部 API なので存在チェック付きで呼び、無ければ何もしない (vendor 差し替えで
  // silently 壊れるより、この保険を諦めるほうが安全)。
  if (document.fonts && document.fonts.ready) {
    document.fonts.ready.then(() => {
      const charSize = term._core && term._core._charSizeService;
      if (!charSize || typeof charSize.measure !== 'function') return;
      const before = charSize.width;
      try { charSize.measure(); } catch (_e) { return; }
      if (charSize.width === before) return;
      if (window.__hyouiDebug) {
        window.__hyouiDebug('info', `cell width changed after font load: ${before} -> ${charSize.width}, refitting`);
      }
      scheduleFit();
    });
  }
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

  // touch 端末では terminal がほぼ全画面を占め、helper textarea の focus を外す
  // タップ先が無い。単一 touch の短い tap だけを focus toggle として扱う。
  // mouse click はこの経路を通らないため、PC の「click で常に focus」は変えない。
  const TOUCH_TAP_MOVE_PX = 10;
  let terminalTouch = null;
  let terminalCloseOnlyClickPending = false;
  const updateTerminalTouchDistance = (touch) => {
    if (!terminalTouch || touch.identifier !== terminalTouch.identifier) return;
    const distance = Math.hypot(
      touch.clientX - terminalTouch.startX,
      touch.clientY - terminalTouch.startY,
    );
    terminalTouch.maxDistance = Math.max(terminalTouch.maxDistance, distance);
  };
  const findTouch = (list, identifier) => {
    for (let i = 0; i < list.length; i++) {
      if (list[i].identifier === identifier) return list[i];
    }
    return null;
  };
  termEl.addEventListener('touchstart', (event) => {
    if (inputPanel.hidden) terminalCloseOnlyClickPending = false;
    if (event.touches.length !== 1 || !term.textarea) {
      terminalTouch = null;
      return;
    }
    const touch = event.touches[0];
    terminalTouch = {
      identifier: touch.identifier,
      startX: touch.clientX,
      startY: touch.clientY,
      maxDistance: 0,
      wasFocused: document.activeElement === term.textarea,
      panelWasOpen: !inputPanel.hidden,
    };
  }, { passive: true });
  termEl.addEventListener('touchmove', (event) => {
    if (!terminalTouch) return;
    const touch = findTouch(event.touches, terminalTouch.identifier);
    if (touch) updateTerminalTouchDistance(touch);
  }, { passive: true });
  termEl.addEventListener('touchend', (event) => {
    if (!terminalTouch) return;
    const state = terminalTouch;
    const touch = findTouch(event.changedTouches, state.identifier);
    if (touch) updateTerminalTouchDistance(touch);
    terminalTouch = null;
    if (!touch || state.maxDistance > TOUCH_TAP_MOVE_PX) return;
    // panel 表示中は panel close をこの tap の唯一の意味にする。入力欄が消えた後に
    // terminal へ暗黙 focus せず、software keyboard が一度閉じる状態を保証する。
    if (state.panelWasOpen) {
      terminalCloseOnlyClickPending = true;
      closePanel({ restoreTerminalFocus: false });
      setTimeout(() => term.blur(), 0);
      return;
    }
    // focus 済み tap は後続 synthetic click が xterm を再 focus するため、この case
    // だけ default を抑止する。移動 gesture と初回 focus は抑止せず既存動作を保つ。
    if (state.wasFocused) event.preventDefault();
    setTimeout(() => {
      if (state.wasFocused) term.blur();
      else term.focus();
    }, 0);
  }, { passive: false });
  termEl.addEventListener('touchcancel', () => { terminalTouch = null; }, { passive: true });
  termEl.addEventListener('pointerdown', () => {
    if (inputPanel.hidden) terminalCloseOnlyClickPending = false;
  }, true);
  // panel open 中の terminal click は xterm の target handler より先に capture し、
  // close-only として確定する。touchend で先に panel を閉じた場合も pending flag で
  // 同じ synthetic click を捕まえ、terminal focus を絶対に通さない。
  termEl.addEventListener('click', (event) => {
    if (inputPanel.hidden && !terminalCloseOnlyClickPending) return;
    terminalCloseOnlyClickPending = false;
    event.preventDefault();
    event.stopImmediatePropagation();
    if (!inputPanel.hidden) closePanel({ restoreTerminalFocus: false });
    term.blur();
    setTimeout(() => term.blur(), 0);
  }, true);

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
  const inputTab = document.getElementById('inputTab');
  const infoTab = document.getElementById('infoTab');
  const inputTabPanel = document.getElementById('inputTabPanel');
  const infoTabPanel = document.getElementById('infoTabPanel');
  const infoDisplaySettings = document.getElementById('infoDisplaySettings');
  const infoAttachMode = document.getElementById('infoAttachMode');
  const infoAttachLeader = document.getElementById('infoAttachLeader');
  const infoLeaderAction = document.getElementById('infoLeaderAction');
  const requestLeaderBtn = document.getElementById('requestLeaderBtn');
  const requestLeaderStatus = document.getElementById('requestLeaderStatus');
  const infoSessionId = document.getElementById('infoSessionId');
  const infoChildPid = document.getElementById('infoChildPid');
  const infoChildState = document.getElementById('infoChildState');
  const infoAttachClients = document.getElementById('infoAttachClients');

  const displaySettingStatus = document.getElementById('displaySettingStatus');
  const sourceLabels = {
    url: ['URL 指定', 'source-url'],
    default: ['default', 'source-default'],
    runtime: ['embed 中に変更', 'source-runtime'],
  };
  const displaySettingSourcesByName = new Map();

  const parseRuntimeNumber = (raw, min, max, integer) => {
    const value = Number(raw.trim());
    if (raw.trim() === '' || !Number.isFinite(value)) throw new Error('数値を入力してください');
    if (value < min || value > max) throw new Error(`${min}〜${max} の範囲で入力してください`);
    return integer ? Math.round(value) : value;
  };
  const loadRuntimeFont = async (family, size) => {
    if (!document.fonts || typeof document.fonts.load !== 'function') return;
    await Promise.race([
      document.fonts.load(`${size}px ${family}`),
      new Promise((resolve) => setTimeout(resolve, FONT_LOAD_TIMEOUT_MS)),
    ]).catch(() => {});
  };
  const redrawForWidthChange = async () => {
    // Unicode provider の切替は既存 buffer cell の保存済み width を遡及変更しない。
    // daemon の screen dump を同じ xterm に再投入して、新 provider で cell を組み直す。
    lastPayload = '';
    await fetchScreen();
  };
  const displaySettings = [
    {
      name: 'unicode', type: 'select', value: () => unicodeVersion,
      options: [['6', '6'], ['11', '11']],
      disabled: () => !unicodeWidthAvailable,
      unavailable: 'Unicode width provider を初期化できないため変更できません',
      apply: async (raw) => {
        const previous = unicodeVersion;
        unicodeVersion = raw;
        try {
          applyUnicodeWidth();
          await redrawForWidthChange();
          return raw;
        } catch (e) {
          unicodeVersion = previous;
          applyUnicodeWidth();
          throw e;
        }
      },
    },
    {
      name: 'ambw', type: 'select', value: () => ambWidth,
      options: [['half', 'half'], ['full', 'full']],
      disabled: () => !unicodeWidthAvailable,
      unavailable: 'Unicode width provider を初期化できないため変更できません',
      apply: async (raw) => {
        const previous = ambWidth;
        ambWidth = raw;
        try {
          applyUnicodeWidth();
          await redrawForWidthChange();
          return raw;
        } catch (e) {
          ambWidth = previous;
          applyUnicodeWidth();
          throw e;
        }
      },
    },
    {
      name: 'fontsize', type: 'number', value: () => String(fontSize), min: 6, max: 40, step: 1,
      apply: async (raw) => {
        const value = parseRuntimeNumber(raw, 6, 40, true);
        await loadRuntimeFont(fontFamily, value);
        term.options.fontSize = value;
        fontSize = value;
        scheduleFit(true);
        return String(value);
      },
    },
    {
      name: 'lineheight', type: 'number', value: () => String(lineHeight), min: 1, max: 2, step: 0.1,
      apply: async (raw) => {
        const value = parseRuntimeNumber(raw, 1, 2, false);
        term.options.lineHeight = value;
        lineHeight = value;
        scheduleFit(true);
        return String(value);
      },
    },
    {
      name: 'scrollback', type: 'number', value: () => String(scrollback), min: 0, max: 100000, step: 1,
      apply: async (raw) => {
        const value = parseRuntimeNumber(raw, 0, 100000, true);
        term.options.scrollback = value;
        scrollback = value;
        return String(value);
      },
    },
    {
      name: 'fontfamily', type: 'text', value: () => fontFamily,
      apply: async (raw) => {
        const value = normalizeFontFamily(raw);
        await loadRuntimeFont(value, fontSize);
        term.options.fontFamily = value;
        fontFamily = value;
        scheduleFit(true);
        return value;
      },
    },
    {
      name: 'bg', type: 'text', value: () => background,
      apply: async (raw) => {
        const value = normalizeColor(raw);
        term.options.theme = { ...term.options.theme, background: value, foreground };
        termEl.style.background = value;
        background = value;
        return value;
      },
    },
    {
      name: 'fg', type: 'text', value: () => foreground,
      apply: async (raw) => {
        const value = normalizeColor(raw);
        term.options.theme = { ...term.options.theme, background, foreground: value };
        foreground = value;
        return value;
      },
    },
  ];

  const setRuntimeSource = (name) => {
    displaySettingSources[name] = 'runtime';
    const source = displaySettingSourcesByName.get(name);
    if (!source) return;
    const [label, className] = sourceLabels.runtime;
    source.className = `source-badge ${className}`;
    source.textContent = label;
  };

  for (const setting of displaySettings) {
    const row = document.createElement('div');
    row.className = 'info-setting-row';
    const label = document.createElement('label');
    label.className = 'info-setting-name';
    label.htmlFor = `displaySetting-${setting.name}`;
    label.textContent = setting.name;
    const control = document.createElement(setting.type === 'select' ? 'select' : 'input');
    control.id = `displaySetting-${setting.name}`;
    control.className = 'info-setting-control';
    if (setting.type === 'select') {
      for (const [value, text] of setting.options) {
        const option = document.createElement('option');
        option.value = value;
        option.textContent = text;
        control.appendChild(option);
      }
    } else {
      control.type = setting.type;
      if (setting.min !== undefined) control.min = String(setting.min);
      if (setting.max !== undefined) control.max = String(setting.max);
      if (setting.step !== undefined) control.step = String(setting.step);
      control.spellcheck = false;
    }
    control.value = setting.value();
    if (setting.disabled?.()) {
      control.disabled = true;
      control.title = setting.unavailable;
      label.title = setting.unavailable;
    }
    const source = document.createElement('span');
    const [sourceLabel, className] = sourceLabels[displaySettingSources[setting.name] || 'default'];
    source.className = `source-badge ${className}`;
    source.textContent = sourceLabel;
    displaySettingSourcesByName.set(setting.name, source);
    control.addEventListener('change', async () => {
      if (control.value === setting.value()) return;
      control.disabled = true;
      displaySettingStatus.textContent = `${setting.name} を反映中…`;
      try {
        const applied = await setting.apply(control.value);
        control.value = applied;
        control.setCustomValidity('');
        setRuntimeSource(setting.name);
        displaySettingStatus.textContent = `${setting.name} を変更しました。外側の reload で元に戻ります。`;
      } catch (e) {
        control.value = setting.value();
        control.setCustomValidity(e.message);
        control.reportValidity();
        displaySettingStatus.textContent = `${setting.name}: ${e.message}`;
      } finally {
        control.disabled = false;
      }
    });
    row.append(label, control, source);
    infoDisplaySettings.appendChild(row);
  }
  infoSessionId.textContent = sid;

  // HTML の `hidden` 属性に加え script 側でも明示的に hide しておく (belt-and-suspenders)。
  // 初回 fetch が成功して child_stopped=true と判明するまでは絶対に表示させない
  // (= false-positive で banner が一瞬映る過渡を潰す。CSS 側の override も別途施した)。
  stoppedBanner.hidden = true;

  let timer = null;
  let lastPayload = '';

  // ---- resize logic ----
  //
  // fit addon は viewport に収まる目標 grid の計算だけに使う。xterm.js の grid を
  // 先に縮めると normal buffer が reflow して、PTY が旧サイズのままなのに表示だけ
  // 折り返される。daemon が resize を受理した応答を受けてから term.resize する。
  // resize off / request 失敗中は旧 grid を維持し、CSS の横スクロールで表示する。
  const LS_AUTO_RESIZE = 'hyoui.session.autoResize';
  try {
    autoResizeEl.checked = localStorage.getItem(LS_AUTO_RESIZE) === '1';
  } catch (_e) { /* private mode 等で失敗しても既定 off */ }
  if (new URLSearchParams(location.search).get('resize') === '1') {
    autoResizeEl.checked = true;
  }

  // vt100 grid は 0 行/列で panic し得る。FitAddon 自身は cols>=2 だが rows>=1
  // なので、極小 iframe でも daemon へ送る寸法を両軸 2 以上に固定する。
  const MIN_RESIZE_COLS = 2;
  const MIN_RESIZE_ROWS = 2;
  let resizePending = null;
  let resizeRunning = false;

  autoResizeEl.addEventListener('change', () => {
    try { localStorage.setItem(LS_AUTO_RESIZE, autoResizeEl.checked ? '1' : '0'); } catch (_e) {}
    if (autoResizeEl.checked) scheduleFit(true);
    else resizePending = null;
  });

  async function postResize(cols, rows) {
    const r = await fetch(`/api/sessions/${encodeURIComponent(sid)}/resize`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ cols, rows }),
    });
    if (!r.ok) {
      const text = await r.text();
      throw new Error(`HTTP ${r.status}: ${text}`);
    }
  }

  async function requestResize(cols, rows) {
    if (wsIsOpen()) return sendResizeOverWs(cols, rows);
    // 初回 WS handshake 中に POST を並走させると、WS が先に leader を取った直後に
    // fallback が 409 になる race を作る。接続確立時の scheduleFit に任せる。
    if (ws && ws.readyState === WebSocket.CONNECTING) {
      throw new Error('WS is still connecting');
    }
    return postResize(cols, rows);
  }

  async function drainResizeQueue() {
    if (resizeRunning) return;
    resizeRunning = true;
    try {
      while (resizePending) {
        const target = resizePending;
        resizePending = null;
        try {
          await requestResize(target.cols, target.rows);
          // 応答待ちの間に新しい viewport 寸法が来た場合、古い成功応答では
          // browser grid を動かさず、次の最新寸法だけを適用する。
          if (resizePending || !autoResizeEl.checked) continue;
          term.resize(target.cols, target.rows);
          if (sizeEl) sizeEl.textContent = `${target.cols}x${target.rows}`;
        } catch (e) {
          if (window.__hyouiDebug) window.__hyouiDebug('warn', 'resize failed: ' + e.message);
        }
      }
    } finally {
      resizeRunning = false;
      if (resizePending) drainResizeQueue();
    }
  }

  function proposeAndQueueResize(force) {
    if (!fitAddon || !autoResizeEl.checked) {
      if (sizeEl) sizeEl.textContent = `${term.cols}x${term.rows}`;
      return;
    }
    let proposed;
    try {
      proposed = fitAddon.proposeDimensions();
    } catch (e) {
      if (window.__hyouiDebug) window.__hyouiDebug('warn', 'fit proposal failed: ' + e.message);
      return;
    }
    if (!proposed || !Number.isFinite(proposed.cols) || !Number.isFinite(proposed.rows)) return;
    const cols = Math.max(MIN_RESIZE_COLS, Math.floor(proposed.cols));
    const rows = Math.max(MIN_RESIZE_ROWS, Math.floor(proposed.rows));
    if (!force && cols === term.cols && rows === term.rows) return;
    resizePending = { cols, rows };
    drainResizeQueue();
  }

  let fitTimer = null;
  function scheduleFit(force = false) {
    if (!fitAddon) return;
    if (fitTimer) clearTimeout(fitTimer);
    fitTimer = setTimeout(() => {
      fitTimer = null;
      proposeAndQueueResize(force);
    }, force ? 0 : 150);
  }

  // 初回も PTY resize 成功前には browser grid を変えない。
  setTimeout(() => {
    if (sizeEl) sizeEl.textContent = `${term.cols}x${term.rows}`;
    if (autoResizeEl.checked) scheduleFit(true);
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
      if (me) {
        infoChildPid.textContent = me.child_pid == null ? '—' : String(me.child_pid);
        infoChildState.textContent = me.status || (stopped ? 'stopped' : 'running');
        infoAttachClients.textContent = me.clients == null ? '—' : String(me.clients);
      } else {
        infoChildPid.textContent = '—';
        infoChildState.textContent = 'not found';
        infoAttachClients.textContent = '—';
      }
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
        pointerType: ev.pointerType || 'mouse',
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
      else if (opts?.onTap) opts.onTap(state.pointerType);
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
      if (ev.detail === 0 && opts?.onTap) opts.onTap('keyboard');
    });
  }

  function selectPanelTab(tab, { autoFocusInput = true } = {}) {
    const showInfo = tab === 'info';
    inputTab.classList.toggle('active', !showInfo);
    infoTab.classList.toggle('active', showInfo);
    inputTab.setAttribute('aria-selected', showInfo ? 'false' : 'true');
    infoTab.setAttribute('aria-selected', showInfo ? 'true' : 'false');
    inputTabPanel.hidden = showInfo;
    infoTabPanel.hidden = !showInfo;
    requestAnimationFrame(() => {
      if (floatPos) applyEdgePos(inputPanel, floatPos);
      if (!showInfo && autoFocusInput) inputText.focus();
    });
  }
  inputTab.addEventListener('click', (event) => {
    selectPanelTab('input', { autoFocusInput: event.pointerType !== 'touch' });
  });
  infoTab.addEventListener('click', () => selectPanelTab('info'));

  function openPanel({ autoFocusInput = true } = {}) {
    // FAB と Panel は「同一浮遊物の折りたたみ/展開」なので、位置引き継ぎは
    // 左上座標でなく edge-relative pos (floatPos) を共有する。左上座標で
    // 引き継ぐと、幅の違う要素間でアンカー辺の判定 (中心点) が反転し、
    // 「右端で開いて閉じたら左端に飛ぶ」バグになる (kawaz 実機 2026-07-23)。
    const pos = floatPos || rectToEdgePos(inputFab.getBoundingClientRect());
    if (document.activeElement === term.textarea) term.blur();
    inputFab.hidden = true;
    inputPanel.hidden = false;
    applyEdgePos(inputPanel, pos);
    savePos(pos);
    requestAnimationFrame(() => {
      if (inputTabPanel.hidden || !autoFocusInput) return;
      inputText.focus();
      const l = inputText.value.length;
      try { inputText.setSelectionRange(l, l); } catch (_e) {}
    });
  }
  function closePanel({ restoreTerminalFocus = true } = {}) {
    // openPanel と対称: edge-relative pos を共有して FAB に適用する
    // (Panel を drag した場合は drag handler が floatPos を更新済み)。
    const pos = floatPos || rectToEdgePos(inputPanel.getBoundingClientRect());
    const active = document.activeElement;
    if (active && inputPanel.contains(active) && typeof active.blur === 'function') active.blur();
    inputPanel.hidden = true;
    inputFab.hidden = false;
    applyEdgePos(inputFab, pos);
    savePos(pos);
    if (restoreTerminalFocus) term.focus();
    else term.blur();
  }
  function togglePanel(pointerType = 'mouse') {
    const isTouch = pointerType === 'touch';
    if (inputPanel.hidden) openPanel({ autoFocusInput: !isTouch });
    else closePanel({ restoreTerminalFocus: !isTouch });
  }

  // FAB: 全体で drag、tap で open。
  attachDrag(inputFab, inputFab, { onTap: togglePanel });

  // Panel: タブ行 (× ボタン除く) で drag。短い tap は各 tab の click として扱い、
  // pointer が閾値を超えて動いた時だけ panel drag に切り替わる。
  const panelTabsRow = inputPanel.querySelector('.panel-tabs-row');
  attachDrag(panelTabsRow, inputPanel, {
    ignoreTarget: (t) => t.closest('.input-panel-close'),
  });

  inputPanelClose.addEventListener('click', (event) => {
    closePanel({ restoreTerminalFocus: event.pointerType !== 'touch' });
  });
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
  const wsTextEncoder = new TextEncoder();
  const wsResizePending = new Map();
  let nextWsResizeId = 1;
  const WS_RESIZE_TIMEOUT_MS = 5000;
  const wsLeaderPending = new Map();
  let nextWsLeaderId = 1;
  const WS_LEADER_TIMEOUT_MS = 5000;

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
  function updateAttachInfo(message) {
    infoAttachMode.textContent = message.mode || 'unknown';
    infoAttachLeader.textContent = message.leader ? 'yes' : 'no';
    // leader でない接続だけに takeover 操作を見せる。成功時は gateway が先に
    // attach.info を再 push するため、この更新だけでボタンが消える。
    infoLeaderAction.hidden = !!message.leader;
    if (message.leader) requestLeaderStatus.textContent = '';
  }

  function wsIsOpen() { return ws && ws.readyState === WebSocket.OPEN; }

  function sendResizeOverWs(cols, rows) {
    if (!wsIsOpen()) return Promise.reject(new Error('WS is not connected'));
    const requestId = nextWsResizeId++;
    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        wsResizePending.delete(requestId);
        reject(new Error('WS resize response timed out'));
      }, WS_RESIZE_TIMEOUT_MS);
      wsResizePending.set(requestId, {
        resolve: () => { clearTimeout(timeout); resolve(); },
        reject: (error) => { clearTimeout(timeout); reject(error); },
      });
      try {
        ws.send(JSON.stringify({ kind: 'resize', requestId, cols, rows }));
      } catch (e) {
        wsResizePending.delete(requestId);
        clearTimeout(timeout);
        reject(e);
      }
    });
  }

  function rejectPendingWsResizes(reason) {
    for (const pending of wsResizePending.values()) pending.reject(new Error(reason));
    wsResizePending.clear();
  }

  async function sendLeaderRequestOverWs() {
    if (!wsIsOpen()) throw new Error('WS is not connected');
    // leader.request 自体は payload を持たない。gateway が takeover 直後に browser の
    // viewport に合う grid で resize できるよう、既存 resize 制御で FitAddon の現在提案値を
    // 先に渡す。non-leader なので通常は reject されるが、gateway は提案値を保持する。
    let cols = term.cols;
    let rows = term.rows;
    if (fitAddon) {
      try {
        const proposed = fitAddon.proposeDimensions();
        if (proposed && Number.isFinite(proposed.cols) && Number.isFinite(proposed.rows)) {
          cols = Math.max(MIN_RESIZE_COLS, Math.floor(proposed.cols));
          rows = Math.max(MIN_RESIZE_ROWS, Math.floor(proposed.rows));
        }
      } catch (_e) { /* current grid fallback */ }
    }
    try { await sendResizeOverWs(cols, rows); } catch (_e) { /* expected before takeover */ }

    const requestId = nextWsLeaderId++;
    await new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        wsLeaderPending.delete(requestId);
        reject(new Error('WS leader response timed out'));
      }, WS_LEADER_TIMEOUT_MS);
      wsLeaderPending.set(requestId, {
        resolve: () => { clearTimeout(timeout); resolve(); },
        reject: (error) => { clearTimeout(timeout); reject(error); },
      });
      try {
        ws.send(JSON.stringify({ kind: 'leader.request', requestId }));
      } catch (e) {
        wsLeaderPending.delete(requestId);
        clearTimeout(timeout);
        reject(e);
      }
    });
    return { cols, rows };
  }

  function rejectPendingWsLeaders(reason) {
    for (const pending of wsLeaderPending.values()) pending.reject(new Error(reason));
    wsLeaderPending.clear();
  }

  requestLeaderBtn.addEventListener('click', async () => {
    requestLeaderBtn.disabled = true;
    requestLeaderStatus.textContent = 'leader を要求中…';
    try {
      const grid = await sendLeaderRequestOverWs();
      term.resize(grid.cols, grid.rows);
      if (sizeEl) sizeEl.textContent = `${grid.cols}x${grid.rows}`;
      requestLeaderStatus.textContent = '';
    } catch (e) {
      requestLeaderStatus.textContent = 'leader 取得失敗: ' + e.message;
    } finally {
      requestLeaderBtn.disabled = false;
    }
  });

  function flushPendingToWs() {
    if (!wsIsOpen() || wsPendingInput.length === 0) return;
    for (const b of wsPendingInput) {
      try { ws.send(b); } catch (_e) { break; }
    }
    wsPendingInput.length = 0;
    wsPendingBytes = 0;
  }

  function sendBytesToWs(bytes) {
    // WS text frame は gateway 制御 JSON 専用。PTY input は文字列も UTF-8 の
    // Uint8Array に変換し、常に binary frame として送る。
    const payload = typeof bytes === 'string' ? wsTextEncoder.encode(bytes) : bytes;
    if (wsIsOpen()) {
      try {
        ws.send(payload);
        return true;
      } catch (e) {
        if (window.__hyouiDebug) window.__hyouiDebug('warn', 'ws.send failed: ' + e.message);
      }
    }
    // queue (上限あり)
    const size = payload.byteLength;
    if (wsPendingBytes + size <= WS_PENDING_MAX) {
      wsPendingInput.push(payload);
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
      if (autoResizeEl.checked) scheduleFit(true);
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
        try {
          const message = JSON.parse(ev.data);
          if (message.kind === 'attach.info') {
            updateAttachInfo(message);
            return;
          }
          if (message.kind === 'resize.result') {
            const pending = wsResizePending.get(message.requestId);
            if (!pending) return;
            wsResizePending.delete(message.requestId);
            if (message.ok) pending.resolve();
            else pending.reject(new Error(message.error || 'resize rejected'));
            return;
          }
          if (message.kind === 'leader.result') {
            const pending = wsLeaderPending.get(message.requestId);
            if (!pending) return;
            wsLeaderPending.delete(message.requestId);
            if (message.ok) pending.resolve();
            else pending.reject(new Error(message.error || 'leader request rejected'));
          }
        } catch (e) {
          if (window.__hyouiDebug) window.__hyouiDebug('warn', 'invalid WS control response: ' + e.message);
        }
      }
    };
    ws.onclose = (ev) => {
      setWsStatus('disconnected (code=' + ev.code + ')');
      infoAttachMode.textContent = 'disconnected';
      infoAttachLeader.textContent = '—';
      infoLeaderAction.hidden = true;
      requestLeaderStatus.textContent = '';
      ws = null;
      rejectPendingWsResizes('WS disconnected before resize completed');
      rejectPendingWsLeaders('WS disconnected before leader request completed');
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
