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
    tbody.innerHTML = '';
    if (!list || list.length === 0) {
      emptyEl.hidden = false;
      return;
    }
    emptyEl.hidden = true;
    for (const s of list) {
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

  autoEl.addEventListener('change', schedule);
  refreshBtn.addEventListener('click', fetchSessions);
  // PWA (standalone) 用の全ページリロード。アドレスバーの再読み込みが無い
  // ホーム画面追加時の retreat 手段。screen refresh (= 一覧再取得) と役割分担。
  const reloadBtn = document.getElementById('reload');
  if (reloadBtn) reloadBtn.addEventListener('click', () => location.reload());
  fetchSessions();
  schedule();
})();
