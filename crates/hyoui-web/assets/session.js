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

  const term = new Terminal({
    cols: COLS,
    rows: ROWS,
    convertEol: false,
    scrollback: 2000,
    disableStdin: true,
    fontFamily: 'Menlo, "DejaVu Sans Mono", Consolas, "Courier New", monospace',
    fontSize: 13,
    theme: { background: '#111', foreground: '#e0e0e0' },
  });
  term.open(document.getElementById('term'));

  const statusEl = document.getElementById('status');
  const autoEl = document.getElementById('auto');
  const refreshBtn = document.getElementById('refresh');
  const inputForm = document.getElementById('inputForm');
  const inputText = document.getElementById('inputText');
  const sendRawBtn = document.getElementById('sendRaw');
  const sendKeyBtn = document.getElementById('sendKey');
  const sendStatus = document.getElementById('sendStatus');

  let timer = null;
  let lastPayload = '';

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
        term.write(text);
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
  refreshBtn.addEventListener('click', fetchScreen);
  fetchScreen();
  schedule();
})();
