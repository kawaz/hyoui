// hyoui web — session list page.
// Polls /api/sessions every few seconds and renders a table.
(() => {
  const REFRESH_MS = 3000;
  const tbody = document.querySelector('#sessions tbody');
  const statusEl = document.getElementById('status');
  const emptyEl = document.getElementById('empty');
  const autoEl = document.getElementById('auto');
  const refreshBtn = document.getElementById('refresh');

  let timer = null;
  // 直近の取得結果。ヘッダクリック時に再 fetch せず並べ替えるために保持する。
  let sessions = [];
  // sort 状態。key === null はサーバ返却順 (= 未ソート)。
  let sortKey = null;
  let sortAsc = true;

  function esc(s) {
    if (s === null || s === undefined) return '';
    return String(s).replace(/[&<>"']/g, (c) => ({
      '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
    }[c]));
  }

  function fmtArgv(a) {
    if (!Array.isArray(a)) return '';
    return a.join(' ');
  }

  // 起動からの経過を短く整形 (= "3d 4h", "5h 12m", "42m", "18s")。
  // 精度は「一覧の目視で状況を掴む」用途、時計としては使わない。
  function fmtUptime(startedUnixMs) {
    if (!startedUnixMs || typeof startedUnixMs !== 'number') return '';
    const sec = Math.max(0, Math.floor((Date.now() - startedUnixMs) / 1000));
    const d = Math.floor(sec / 86400);
    const h = Math.floor((sec % 86400) / 3600);
    const m = Math.floor((sec % 3600) / 60);
    if (d > 0) return `${d}d ${h}h`;
    if (h > 0) return `${h}h ${m}m`;
    if (m > 0) return `${m}m`;
    return `${sec}s`;
  }

  function fmtStarted(startedUnixMs) {
    if (!startedUnixMs) return '';
    try { return new Date(startedUnixMs).toISOString(); } catch { return ''; }
  }

  // ソート用の比較値。数値カラムは数値、それ以外は小文字文字列を返す。
  // uptime は「経過時間の長さ」で比べる (= started_unix_ms の大小とは逆向き)。
  function sortValue(s, key) {
    switch (key) {
      case 'session_id': return String(s.session_id || '').toLowerCase();
      case 'namespace': return String(s.namespace || '').toLowerCase();
      case 'status': return (s.status === 'stopped' || s.child_stopped)
        ? 'stopped' : String(s.status || '').toLowerCase();
      case 'uptime': return s.started_unix_ms ? Date.now() - s.started_unix_ms : -1;
      case 'clients': return Number(s.clients ?? -1);
      case 'suspend': return String(s.on_child_suspend || '').toLowerCase();
      case 'version': return String(s.daemon_version || '').toLowerCase();
      case 'argv': return fmtArgv(s.argv).toLowerCase();
      case 'cwd': return String(s.cwd || '').toLowerCase();
      default: return '';
    }
  }

  function sorted(list) {
    if (!sortKey) return list;
    const dir = sortAsc ? 1 : -1;
    return list.slice().sort((a, b) => {
      const va = sortValue(a, sortKey);
      const vb = sortValue(b, sortKey);
      if (va < vb) return -dir;
      if (va > vb) return dir;
      return 0;
    });
  }

  // ヘッダの矢印表示と aria-sort を現在の状態に合わせる。
  function syncHeaders() {
    for (const th of document.querySelectorAll('#sessions thead th[data-sort]')) {
      const active = th.dataset.sort === sortKey;
      th.classList.toggle('sorted', active);
      th.dataset.dir = active ? (sortAsc ? 'asc' : 'desc') : '';
      th.setAttribute('aria-sort', active ? (sortAsc ? 'ascending' : 'descending') : 'none');
    }
  }

  async function fetchSessions() {
    statusEl.textContent = 'fetching…';
    try {
      const r = await fetch('/api/sessions', { cache: 'no-store' });
      if (!r.ok) throw new Error('HTTP ' + r.status);
      const list = await r.json();
      render(list);
      statusEl.textContent = 'updated ' + new Date().toLocaleTimeString();
    } catch (e) {
      statusEl.textContent = 'error: ' + e.message;
    }
  }

  function render(list) {
    sessions = Array.isArray(list) ? list : [];
    tbody.innerHTML = '';
    syncHeaders();
    if (sessions.length === 0) {
      emptyEl.hidden = false;
      return;
    }
    emptyEl.hidden = true;
    for (const s of sorted(sessions)) {
      const tr = document.createElement('tr');
      const isLive = s.status === 'live';
      const isStopped = s.status === 'stopped' || !!s.child_stopped;
      tr.className = 'row-' + esc(s.status || 'unknown');
      const link = isLive || isStopped
        ? `<a href="/sessions/${encodeURIComponent(s.session_id)}">${esc(s.session_id)}</a>`
        : esc(s.session_id);
      const statusCell = isStopped
        ? `<span class="badge badge-stopped" title="child is stopped (SIGSTOP)">⏸ stopped</span>`
        : (isLive ? `<span class="badge badge-live" title="live">● live</span>` : esc(s.status));
      const uptime = fmtUptime(s.started_unix_ms);
      const startedTitle = fmtStarted(s.started_unix_ms);
      // suspend policy: null (旧 daemon) or "notify" / "auto-resume"。
      const suspend = s.on_child_suspend
        ? `<span title="${esc(s.on_child_suspend)}">${esc(s.on_child_suspend)}</span>`
        : `<span class="muted" title="旧 daemon or 未報告">-</span>`;
      const version = s.daemon_version
        ? `<code title="daemon binary version">${esc(s.daemon_version)}</code>`
        : `<span class="muted" title="旧 daemon or 未報告">-</span>`;
      const clientsCell = (s.clients ?? '') === '' ? '' :
        `<span title="attached clients">${esc(s.clients)}</span>`;
      const argvStr = fmtArgv(s.argv);
      const cwdStr = s.cwd || '';
      tr.innerHTML = [
        `<td>${link}</td>`,
        `<td>${esc(s.namespace)}</td>`,
        `<td>${statusCell}</td>`,
        `<td class="uptime" title="started ${esc(startedTitle)}">${esc(uptime)}</td>`,
        `<td>${clientsCell}</td>`,
        `<td class="suspend">${suspend}</td>`,
        `<td>${version}</td>`,
        `<td class="argv" title="${esc(argvStr)}"><code>${esc(argvStr)}</code></td>`,
        `<td class="cwd" title="${esc(cwdStr)}">${esc(cwdStr)}</td>`,
      ].join('');
      tbody.appendChild(tr);
    }
  }

  function schedule() {
    if (timer) { clearInterval(timer); timer = null; }
    if (autoEl.checked) {
      timer = setInterval(fetchSessions, REFRESH_MS);
    }
  }

  // ヘッダクリックでソート。同じ列を再クリックすると昇順/降順をトグルする。
  // 再 fetch はせず、保持済みの直近取得結果を並べ替えるだけ。
  for (const th of document.querySelectorAll('#sessions thead th[data-sort]')) {
    th.addEventListener('click', () => {
      const key = th.dataset.sort;
      if (sortKey === key) sortAsc = !sortAsc;
      else { sortKey = key; sortAsc = true; }
      render(sessions);
    });
  }

  autoEl.addEventListener('change', schedule);
  refreshBtn.addEventListener('click', fetchSessions);
  // PWA (standalone) 用の全ページリロード。アドレスバーの再読み込みが無い
  // ホーム画面追加時の retreat 手段。screen refresh (= 一覧再取得) と役割分担。
  const reloadBtn = document.getElementById('reload');
  if (reloadBtn) reloadBtn.addEventListener('click', () => location.reload());
  fetchSessions();
  schedule();
})();
