// hyoui web — passkey で endpoint に入る (DR-0036 決定 1 / 決定 2 / 決定 5)。
//
// ## ログインの出しかた
//
// **401 を受けたらページ内に overlay を出す。redirect しない** (決定 1) — iframe の
// 中で redirect が起きると、親 (ccmsg) の Terminal タブがログインページに化けて、
// 何が起きたか読めなくなる。
//
// ## token の持ちかた
//
// access は **メモリだけ** に置く (決定 5)。localStorage には書かない。refresh token は
// httpOnly cookie で server が持つので、この file は触らない (= 触れない)。
// 複数タブの協調は `auth-share.js` に分けてある。
//
// ## 登録
//
// 招待 URL (`<endpoint>#register=<jwt>`) を開いた時だけ登録 UI を出す。jwt は
// **fragment にあるので server に送られない**。6 桁コードは URL に入っていないので、
// 利用者が CLI の表示を見て入れる。**登録は top-level でだけ走らせる** (決定 6)。
(() => {
  const { resolve, httpError } = window.hyouiContract;
  const { createAuthShare } = window.hyouiAuthShare;

  // access の残り寿命がこの割合を切ったら先に延ばす (決定 5)。
  const REFRESH_AT_REMAINING = 0.1;
  // 延長 timer の最小間隔 (= 時計のずれで連打しない)。
  const MIN_REFRESH_DELAY_MS = 5_000;

  // ---- base64url ↔ bytes ----
  //
  // server は crate が組んだ options をそのまま返す (= `challenge` / `user.id` /
  // `allowCredentials[].id` は base64url の文字列)。WebAuthn の API は ArrayBuffer を
  // 取るので、境界で変換する。

  function fromBase64Url(value) {
    const padded = value.replace(/-/g, '+').replace(/_/g, '/');
    const binary = atob(padded + '='.repeat((4 - (padded.length % 4)) % 4));
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
    return bytes;
  }

  function toBase64Url(buffer) {
    const bytes = new Uint8Array(buffer);
    let binary = '';
    for (const byte of bytes) binary += String.fromCharCode(byte);
    return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
  }

  // options の base64url を ArrayBuffer に開く。crate の形 (= WebAuthn の JSON 表現)
  // に現れる field だけを見る。
  function decodeOptions(options) {
    const decoded = { ...options };
    if (typeof decoded.challenge === 'string') decoded.challenge = fromBase64Url(decoded.challenge);
    if (decoded.user && typeof decoded.user.id === 'string') {
      decoded.user = { ...decoded.user, id: fromBase64Url(decoded.user.id) };
    }
    for (const key of ['allowCredentials', 'excludeCredentials']) {
      if (Array.isArray(decoded[key])) {
        decoded[key] = decoded[key].map((item) => ({
          ...item,
          id: typeof item.id === 'string' ? fromBase64Url(item.id) : item.id,
        }));
      }
    }
    return decoded;
  }

  // credential を server が読める JSON にする。
  function encodeCredential(credential) {
    const response = credential.response;
    const body = {
      id: credential.id,
      rawId: toBase64Url(credential.rawId),
      type: credential.type,
      response: {
        clientDataJSON: toBase64Url(response.clientDataJSON),
      },
      extensions: credential.getClientExtensionResults
        ? credential.getClientExtensionResults()
        : {},
    };
    if (response.attestationObject) {
      body.response.attestationObject = toBase64Url(response.attestationObject);
    }
    if (response.authenticatorData) {
      body.response.authenticatorData = toBase64Url(response.authenticatorData);
      body.response.signature = toBase64Url(response.signature);
      // **userHandle は record と毎回照合される** (決定 2)。`residentKey: required`
      // なので常に載る。
      body.response.userHandle = response.userHandle ? toBase64Url(response.userHandle) : null;
    }
    return body;
  }

  // ---- /auth/* ----

  function createClient(endpoint) {
    const endpointString = typeof endpoint === 'string' ? endpoint : endpoint.href;

    async function post(path, body) {
      const response = await fetch(resolve(endpoint, path), {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        // refresh cookie を送る (= 同一 origin なので既定でも送られるが明示する)。
        credentials: 'same-origin',
        body: JSON.stringify({ endpoint: endpointString, ...body }),
      });
      if (!response.ok) throw await httpError(response);
      return response.json();
    }

    return {
      endpoint: endpointString,
      challenge(extra) {
        return post('auth/challenge', extra);
      },
      assert(body) {
        return post('auth/assert', body);
      },
      register(body) {
        return post('auth/register', body);
      },
      refresh() {
        return post('auth/refresh', {});
      },
    };
  }

  // ---- 認証の状態 ----

  function createAuth(endpoint) {
    const client = createClient(endpoint);
    const share = createAuthShare({
      endpoint: client.endpoint,
      locks: typeof navigator !== 'undefined' ? navigator.locks ?? null : null,
      makeChannel:
        typeof BroadcastChannel !== 'undefined' ? (name) => new BroadcastChannel(name) : null,
      refresh: async () => {
        const session = await client.refresh();
        return {
          accessToken: session.access_token,
          expiresAtMs: Date.parse(session.expires_at),
          sub: session.sub,
        };
      },
    });

    let signInPromise = null;
    let refreshTimer = null;
    const overlay = createOverlay();

    // **招待 token は createAuth の時点で読む** (= 同期)。読んだら URL から外す
    // (リロードや共有で再利用されないように)。ここで先に取っておかないと、
    // ページの最初の `/api/*` が返す 401 とどちらが先かで出す UI が変わってしまう。
    const invitation = takeRegistrationToken();
    let registrationPromise = null;

    // 期限の 90% 時点で先に延ばす。**切るのは延長を怠った接続だけ**なので、
    // 画面が周期的に瞬くことはない (決定 5)。
    function scheduleRefresh(expiresAtMs) {
      if (refreshTimer !== null) clearTimeout(refreshTimer);
      const lifetime = expiresAtMs - Date.now();
      const delay = Math.max(MIN_REFRESH_DELAY_MS, lifetime * (1 - REFRESH_AT_REMAINING));
      if (!Number.isFinite(delay)) return;
      refreshTimer = setTimeout(() => {
        refreshTimer = null;
        // **`ensure` ではなく `refreshAhead`**。`ensure` は「期限内なら何もしない」
        // なので、予定した延長をそれで呼ぶと何も起きず、期限まで放置される。
        share.refreshAhead().catch(() => {
          // 延長に失敗したら次の 401 でログイン UI に落ちる。ここでは騒がない。
        });
      }, delay);
    }

    share.onAccess((access) => {
      scheduleRefresh(access.expiresAtMs);
      for (const listener of accessListeners) listener(access.value);
    });

    const accessListeners = new Set();

    // **拒否された時の順序** (reference 手順 4): 他タブの値 → ロック付き refresh →
    // 再認証。backoff ループには入らない。
    async function acquire(rejected) {
      try {
        const access = rejected ? await share.renew(rejected) : await share.ensure();
        if (access) return access.value;
      } catch (_error) {
        // refresh が通らない = cookie が無い / 失効した。再認証へ落ちる。
      }
      // 招待 URL で開かれているなら、ログインではなく**登録**が先である
      // (= 登録が即サインインになる、決定 2)。どちらの経路から先に来ても
      // 同じ 1 本の promise を待つ。
      const registration = ensureRegistration();
      if (registration) {
        const session = await registration;
        if (session) return session.access_token;
      }
      return signIn();
    }

    /** passkey で入る。overlay を出し、利用者の操作を待つ。 */
    function signIn() {
      if (signInPromise) return signInPromise;
      signInPromise = overlay
        .promptSignIn(async () => {
          const challenge = await client.challenge({ purpose: 'assert' });
          const credential = await navigator.credentials.get({
            publicKey: decodeOptions(challenge.options),
          });
          if (!credential) throw new Error('認証がキャンセルされました');
          const session = await client.assert({
            challenge_id: challenge.challenge_id,
            credential: encodeCredential(credential),
          });
          return session;
        })
        .then((session) => {
          share.adopt(session.access_token, Date.parse(session.expires_at), session.sub);
          return session.access_token;
        })
        .finally(() => {
          signInPromise = null;
        });
      return signInPromise;
    }

    /**
     * 招待 URL で開かれた時の登録。`#register=<jwt>` が無ければ null を返す。
     *
     * **何度呼んでも走るのは 1 回**で、同じ promise を返す (= bootstrap と
     * `acquire` の両方から呼ばれる)。
     */
    function ensureRegistration() {
      if (!invitation) return null;
      if (!registrationPromise) registrationPromise = runRegistration();
      return registrationPromise;
    }

    async function runRegistration() {
      const jwt = invitation;
      if (window.top !== window.self) {
        // 登録は top-level でだけ走らせる (決定 6)。iframe では誘導だけ出す。
        overlay.showTopLevelNotice();
        return null;
      }
      const session = await overlay.promptRegister(async (code, label) => {
        const challenge = await client.challenge({ purpose: 'register', jwt });
        const credential = await navigator.credentials.create({
          publicKey: decodeOptions(challenge.options),
        });
        if (!credential) throw new Error('登録がキャンセルされました');
        return client.register({
          challenge_id: challenge.challenge_id,
          jwt,
          code,
          credential: encodeCredential(credential),
          device_label: label || null,
        });
      });
      // 登録が即サインインになる (決定 2)。
      share.adopt(session.access_token, Date.parse(session.expires_at), session.sub);
      return session;
    }

    return {
      /** 今の access (無ければ null)。 */
      current() {
        const access = share.current();
        return access ? access.value : null;
      },
      /** 期限内の access を 1 本取る。無ければ refresh か再認証まで進む。 */
      acquire,
      signIn,
      /** 招待 URL で開かれていれば登録を走らせる。無ければ null。 */
      registerFromFragment: ensureRegistration,
      /** 招待 URL で開かれているか (= 起動時に登録 UI を出すべきか)。 */
      hasInvitation() {
        return invitation !== null;
      },
      /** access が差し替わった時に呼ばれる (= WS の延長に使う)。 */
      onAccess(listener) {
        accessListeners.add(listener);
        return () => accessListeners.delete(listener);
      },
      /** `Authorization: Bearer` を付けて fetch し、401 なら 1 度だけ取り直す。 */
      async fetch(path, init = {}) {
        const access = await acquire();
        const send = (token) =>
          fetch(resolve(endpoint, path), {
            ...init,
            credentials: 'same-origin',
            headers: { ...(init.headers ?? {}), authorization: `Bearer ${token}` },
          });
        let response = await send(access);
        if (response.status !== 401) return response;
        // 1 度だけ取り直す。同じ値の再提示は繰り返さない。
        const renewed = await acquire(access);
        response = await send(renewed);
        return response;
      },
      /** WS の subprotocol (決定 5)。access を RFC 6455 の token として運ぶ。 */
      wsProtocol(access) {
        return `hyoui.token.${access}`;
      },
      overlay,
    };
  }

  // 招待 token を fragment から読み、**URL から外して**返す。
  //
  // fragment は server にも proxy log にも Referer にも乗らない (決定 2)。外すのは
  // リロード / bookmark / 共有で同じ URL が再利用されるのを防ぐためで、token 自体は
  // 1 回しか使えない (= 登録が通れば jti が消費される)。
  function takeRegistrationToken() {
    const hash = location.hash.startsWith('#') ? location.hash.slice(1) : location.hash;
    if (!hash) return null;
    const params = new URLSearchParams(hash);
    const jwt = params.get('register');
    if (!jwt || jwt.length === 0) return null;
    history.replaceState(null, '', location.pathname + location.search);
    return jwt;
  }

  // ---- overlay ----
  //
  // ページの上に重ねるだけで、ページ自身は残す (= iframe でも壊れない、決定 1)。

  function createOverlay() {
    let root = null;

    function ensureRoot() {
      if (root) return root;
      root = document.createElement('div');
      root.className = 'auth-overlay';
      root.setAttribute('role', 'dialog');
      root.setAttribute('aria-modal', 'true');
      document.body.appendChild(root);
      return root;
    }

    function clear() {
      if (root) root.remove();
      root = null;
    }

    function panel(title) {
      const host = ensureRoot();
      host.textContent = '';
      const box = document.createElement('div');
      box.className = 'auth-panel';
      const heading = document.createElement('h2');
      heading.textContent = title;
      box.appendChild(heading);
      host.appendChild(box);
      return box;
    }

    function message(box, text, kind) {
      const paragraph = document.createElement('p');
      paragraph.className = kind === 'error' ? 'auth-error' : 'auth-note';
      paragraph.textContent = text;
      box.appendChild(paragraph);
      return paragraph;
    }

    return {
      /** 「passkey で入る」ボタンを出し、押されたら `run` を走らせる。 */
      promptSignIn(run) {
        const box = panel('このセッションは passkey で保護されています');
        message(box, '登録した端末で本人確認してください。');
        const button = document.createElement('button');
        button.type = 'button';
        button.className = 'auth-primary';
        button.textContent = 'passkey で続ける';
        box.appendChild(button);
        const status = message(box, '');
        // iframe で `get` が走らない構成 (`allow` 属性が無い) のための逃げ道。
        if (window.top !== window.self) {
          const link = document.createElement('a');
          link.href = location.href;
          link.target = '_blank';
          link.rel = 'noopener';
          link.className = 'auth-secondary';
          link.textContent = '別タブで開く';
          box.appendChild(link);
        }
        return new Promise((resolve) => {
          button.addEventListener('click', async () => {
            button.disabled = true;
            status.className = 'auth-note';
            status.textContent = '本人確認を待っています…';
            try {
              const session = await run();
              clear();
              resolve(session);
            } catch (error) {
              button.disabled = false;
              status.className = 'auth-error';
              status.textContent = `入れませんでした: ${error.message}`;
            }
          });
        });
      },

      /** 6 桁コードの入力を受けて登録する。 */
      promptRegister(run) {
        const box = panel('この端末を登録します');
        message(box, 'ホストの `hyoui web passkey add` が表示した 6 桁のコードを入れてください。');
        const code = document.createElement('input');
        code.type = 'text';
        code.inputMode = 'numeric';
        code.autocomplete = 'one-time-code';
        code.maxLength = 6;
        code.className = 'auth-code';
        code.setAttribute('aria-label', '6 桁のコード');
        box.appendChild(code);
        const label = document.createElement('input');
        label.type = 'text';
        label.placeholder = 'この端末の名前 (任意)';
        label.className = 'auth-label';
        label.setAttribute('aria-label', 'この端末の名前');
        box.appendChild(label);
        const button = document.createElement('button');
        button.type = 'button';
        button.className = 'auth-primary';
        button.textContent = '登録する';
        box.appendChild(button);
        const status = message(box, '');
        code.focus();
        return new Promise((resolve) => {
          const submit = async () => {
            if (code.value.length !== 6) {
              status.className = 'auth-error';
              status.textContent = 'コードは 6 桁です。';
              return;
            }
            button.disabled = true;
            status.className = 'auth-note';
            status.textContent = '登録しています…';
            try {
              const session = await run(code.value, label.value);
              clear();
              resolve(session);
            } catch (error) {
              button.disabled = false;
              status.className = 'auth-error';
              // 失敗の理由は server が分けない (決定 5)。再発行の誘導だけ出す。
              status.textContent =
                `登録できませんでした: ${error.message} — ` +
                'コードが違う場合は残り回数があります。5 回間違えるか 10 分経つと ' +
                'URL が失効するので、ホストで `hyoui web passkey add` を打ち直してください。';
            }
          };
          button.addEventListener('click', submit);
          code.addEventListener('keydown', (event) => {
            if (event.key === 'Enter') submit();
          });
        });
      },

      /** iframe 内で登録 URL を開かれた時の誘導。 */
      showTopLevelNotice() {
        const box = panel('登録は別タブで行います');
        message(box, 'この画面は埋め込みなので、登録の本人確認が走りません。');
        const link = document.createElement('a');
        link.href = location.href;
        link.target = '_blank';
        link.rel = 'noopener';
        link.className = 'auth-primary';
        link.textContent = '別タブで開く';
        box.appendChild(link);
      },

      /** 失効を伝える (= `auth.extend` が `ok:false` を返した時)。 */
      showRevoked() {
        const box = panel('この認証セッションは失効しました');
        message(box, 'ホストで失効させたか、別の端末で使い直された可能性があります。');
        const button = document.createElement('button');
        button.type = 'button';
        button.className = 'auth-primary';
        button.textContent = '再読み込み';
        button.addEventListener('click', () => location.reload());
        box.appendChild(button);
      },

      close: clear,
    };
  }

  window.hyouiAuth = { createAuth, fromBase64Url, toBase64Url, decodeOptions, encodeCredential };
})();
