// hyoui web — 複数タブで 1 本の access を共有しながら refresh する
// (DR-0036 決定 5、reference `auth-patterns/multi-tab-token-refresh`)。
//
// ## なぜ協調が要るか
//
// access は「ページのもの」ではなく「利用者の token family のもの」なので、同じ人が
// 開いた複数のタブが同じ 1 本を提示する。タブ A が refresh して family の access が
// 新しくなると、B が持っている値は拒否され、B も refresh し、今度は A が拒否される。
// 開いているタブの数だけこの往復が続く。
//
// 止め方は 2 つ要る: **クライアント側で refresh を 1 本に絞る** (この file) と、
// **サーバ側で差し替えを減らす** (`/auth/refresh` の据え置き、決定 5)。協調が効かない
// 環境があるので前者だけでは足りず、後者だけでは rotate の回数が減らない。
//
// ## この file が持たないもの
//
// DOM も fetch も `navigator` も直接触らない。**依存は全部引数で受ける** —
// ブラウザでは実物 (`navigator.locks` / `BroadcastChannel` / `Date.now`) を、test では
// fake を渡す。7 性質 (reference) を test で固定できる形にするためである。
//
// ## 永続化しない
//
// 配るのは**メモリからメモリへ**だけで、storage には書かない。書いた瞬間、その origin
// で走る任意のスクリプトから読める値になる。
(() => {
  // 問い合わせの打ち切り。同一 origin の message 往復に主スレッドの混み具合を足した
  // 程度で、**待ち切るのは「このセッションで開いているタブが自分だけ」の時だけ**。
  // その時に遅らせているのはどうせネットワーク往復を使う refresh なので損がない。
  const DEFAULT_ASK_TIMEOUT_MS = 50;

  // 1 つの認証セッション (endpoint + sub) に対する協調。
  //
  // `key` に sub を含めるのは、1 つの origin が複数の endpoint を、1 つの endpoint が
  // 複数の人を持ちうるため。id を含まない固定名にすると無関係なタブ同士が待ち合う。
  function createAuthShare(options) {
    const {
      endpoint,
      locks = null,
      makeChannel = null,
      now = () => Date.now(),
      refresh,
      askTimeoutMs = DEFAULT_ASK_TIMEOUT_MS,
    } = options;
    if (typeof refresh !== 'function') throw new TypeError('refresh function is required');
    if (!endpoint) throw new TypeError('endpoint is required');

    let sub = options.sub ?? null;
    // 立っている access。`{ value, expiresAtMs }` か null。
    let standing = null;
    let channel = null;
    const listeners = new Set();

    // **ロック名には sub を含める。** 1 つの endpoint が複数の人を持ちうるので、
    // sub を含まない固定名にすると無関係なタブ同士が待ち合う。sub が分かる前は
    // endpoint だけの名前で待ち、確定した時点で張り替える (reference 手順 5)。
    function lockName() {
      return sub === null
        ? `hyoui.auth.refresh:${endpoint}`
        : `hyoui.auth.refresh:${endpoint}:${sub}`;
    }

    // **channel は endpoint 単位で 1 本にする。**
    //
    // Design rationale: reference は channel も sub で張り替えると書くが、それだと
    // **開いたばかりのタブ (= まだ sub を知らない) が、sub を知っているタブに
    // 届かない** — 名前が違うので ask も offer も相手に出ない。問い合わせで既に
    // 立っている値を受け取る (reference 手順 3 / 性質 4) はこの経路が要るので、
    // 待ち合わせ場所は endpoint 単位にして、**誰の値かは message の `sub` で
    // 判定する** (reference が「配るメッセージには sub を載せる」と書くのは、
    // 名前から読めない相手が居るからである)。
    //
    // sub による分離は落ちない: 自分の sub が確定している側は他人の offer を捨て、
    // refresh の排他は sub を含む lock 名が担う。
    function channelName() {
      return `hyoui.auth:${endpoint}`;
    }

    function isLive(access) {
      return access !== null && access.expiresAtMs > now();
    }

    function adopt(access) {
      if (!isLive(access)) return null;
      standing = access;
      for (const listener of listeners) listener(access);
      return access;
    }

    function openChannel() {
      if (!makeChannel) return;
      channel = makeChannel(channelName());
      if (!channel) return;
      channel.onmessage = (event) => {
        const message = event && event.data;
        if (!message || typeof message !== 'object') return;
        if (message.kind === 'ask') {
          // 期限内の値を持っていれば答える。持っていなければ黙る (= 答えが無い
          // ことは「誰も持っていない」と区別しない、reference 手順 3)。
          if (isLive(standing)) offer(standing);
          return;
        }
        if (message.kind === 'offer') {
          // sub が違う相手とは調停しない。endpoint だけの key で待っている間は
          // 相手の sub を name から読めないので、message に載った sub で判定する。
          if (sub !== null && message.sub !== sub) return;
          const offered = { value: message.accessToken, expiresAtMs: message.expiresAtMs };
          // **期限切れの値は受けた側も使わない** (reference の性質)。
          if (!isLive(offered)) return;
          if (sub === null && typeof message.sub === 'string') sub = message.sub;
          adopt(offered);
        }
      };
    }

    function closeChannel() {
      if (channel && typeof channel.close === 'function') channel.close();
      channel = null;
    }

    function post(message) {
      if (channel && typeof channel.postMessage === 'function') channel.postMessage(message);
    }

    function offer(access) {
      post({ kind: 'offer', sub, accessToken: access.value, expiresAtMs: access.expiresAtMs });
    }

    // 他のタブが期限内の access を持っていれば受け取る。無ければ null。
    function ask() {
      if (!channel) return Promise.resolve(null);
      return new Promise((resolve) => {
        let settled = false;
        const stop = onAccess((access) => {
          if (settled) return;
          settled = true;
          stop();
          resolve(access);
        });
        post({ kind: 'ask' });
        setTimeout(() => {
          if (settled) return;
          settled = true;
          stop();
          resolve(isLive(standing) ? standing : null);
        }, askTimeoutMs);
      });
    }

    async function refreshNow() {
      const result = await refresh();
      if (!result) return null;
      if (typeof result.sub === 'string') sub = result.sub;
      const access = { value: result.accessToken, expiresAtMs: result.expiresAtMs };
      const adopted = adopt(access);
      if (adopted) offer(adopted);
      return adopted;
    }

    // **refresh は排他ロックの中でだけ行う。** ロックを取ったら、まず「持って
    // いる?」と尋ねる — ロックと message は別々の queue で渡るので、前のタブが
    // 配ったものがロックを取った時点で届いているとは限らない (reference 手順 3)。
    async function refreshUnderLock() {
      if (!locks || typeof locks.request !== 'function') {
        // **ロックが無い環境では各タブが自分で refresh する。** 協調はこの挙動を
        // 改善するものであって、成立の前提ではない (reference 手順 6)。
        return refreshNow();
      }
      return locks.request(lockName(), async () => {
        if (isLive(standing)) return standing;
        const offered = await ask();
        if (isLive(offered)) return offered;
        return refreshNow();
      });
    }

    function onAccess(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    }

    openChannel();

    return {
      /** 立っている access (期限内なら)。無ければ null。 */
      current() {
        return isLive(standing) ? standing : null;
      },
      /** 期限内の access を 1 本返す。無ければ協調のうえ refresh する。 */
      async ensure() {
        if (isLive(standing)) return standing;
        return refreshUnderLock();
      },
      /**
       * 期限が近いので、**立っている値があっても**取り直す (決定 5 の 90% 時点)。
       *
       * `ensure` は「期限内なら何もしない」なので、予定した延長をそれで呼ぶと
       * 何も起きない (= 期限まで放置されて画面が切れる)。先回りの延長はこちらを使う。
       * 取り直しは協調の中で行うので、他のタブが既に新しい値を持っていれば
       * server は叩かない。
       */
      async refreshAhead() {
        standing = null;
        return refreshUnderLock();
      },
      /** 拒否された値を捨てて取り直す (= handshake が 401 を返した時)。 */
      async renew(rejected) {
        if (standing && rejected && standing.value === rejected) standing = null;
        return refreshUnderLock();
      },
      /** ログイン直後の access を採り入れて他のタブへ配る。 */
      adopt(accessToken, expiresAtMs, newSub) {
        if (typeof newSub === 'string') setSub(newSub);
        const access = adopt({ value: accessToken, expiresAtMs });
        if (access) offer(access);
        return access;
      },
      /** sub が確定したらロック名と channel を張り替える (reference 手順 5)。 */
      setSub,
      /** 今の sub。 */
      sub() {
        return sub;
      },
      /** access が変わった時に呼ばれる。返り値で解除。 */
      onAccess,
      /** 協調をやめる (= ページ離脱時)。 */
      close() {
        closeChannel();
        listeners.clear();
      },
      // test / 診断用。lock 名と channel 名が key を含むことを外から見る。
      names() {
        return { lock: lockName(), channel: channelName() };
      },
    };

    function setSub(newSub) {
      if (newSub === sub) return;
      // channel の名前は endpoint 単位で変わらない (= 張り替え不要)。変わるのは
      // 排他ロックの名前だけで、次の refresh から新しい名前で待つ。
      sub = newSub;
    }
  }

  const api = { createAuthShare, DEFAULT_ASK_TIMEOUT_MS };
  // ブラウザでは global に置き、node の test では require で読む (= 同じ 1 本の
  // 実装を両方から使う。写しを作らない)。
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  if (typeof window !== 'undefined') window.hyouiAuthShare = api;
})();
