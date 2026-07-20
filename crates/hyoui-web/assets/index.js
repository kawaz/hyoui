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
      tr.className = 'row-' + esc(s.status || 'unknown');
      const link = isLive || s.status === 'stopped'
        ? `<a href="/sessions/${encodeURIComponent(s.session_id)}">${esc(s.session_id)}</a>`
        : esc(s.session_id);
      tr.innerHTML = [
        `<td>${link}</td>`,
        `<td>${esc(s.namespace)}</td>`,
        `<td>${esc(s.status)}</td>`,
        `<td>${esc(s.clients ?? '')}</td>`,
        `<td class="argv"><code>${esc(fmtArgv(s.argv))}</code></td>`,
        `<td class="cwd">${esc(s.cwd || '')}</td>`,
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
  fetchSessions();
  schedule();
})();
