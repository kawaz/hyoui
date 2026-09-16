// DR-0036 W2-5 の gate: reference `auth-patterns/multi-tab-token-refresh` が
// 「テストで固定する」と列挙した 7 性質を、そのまま 7 つの test にする。
//
// 対象は `assets/auth-share.js` の協調 core。**assets 側を本物のまま読む**
// (= 写しを作らない)。`node --test` で回し、`just test-js` / CI の js job が呼ぶ。
//
// 実物の `navigator.locks` / `BroadcastChannel` はここでは使わない。使うと
// 「2 タブが同時に切れた」のような時間関係を組めないので、同じ契約の fake を置く。
// 実ブラウザでの通し (登録 → 認証 → refresh) は playwright 側の責務である。

const test = require('node:test');
const assert = require('node:assert/strict');

const { createAuthShare } = require('../../assets/auth-share.js');

// ---- fake: Web Locks ----
//
// 名前ごとに 1 本の直列キュー。`request` は前の保持者の解放を待つ。
function createLockManager() {
  const tails = new Map();
  return {
    held: [],
    request(name, callback) {
      this.held.push(name);
      const previous = tails.get(name) ?? Promise.resolve();
      const run = previous.then(() => callback());
      // 失敗しても次の待ち手を止めない (= 実物と同じ)。
      tails.set(
        name,
        run.then(
          () => undefined,
          () => undefined,
        ),
      );
      return run;
    },
  };
}

// ---- fake: BroadcastChannel ----
//
// 名前ごとの hub。**同じ名前の相手にだけ届く** ので、endpoint / sub が違うタブが
// 調停しないことがこの構造で表れる。
function createChannelHub() {
  const byName = new Map();
  return {
    make(name) {
      const peers = byName.get(name) ?? new Set();
      byName.set(name, peers);
      const channel = {
        name,
        onmessage: null,
        closed: false,
        postMessage(data) {
          for (const peer of peers) {
            if (peer === channel || peer.closed) continue;
            // 実物と同じく非同期に届く (= lock と message が別 queue で渡る)。
            setTimeout(() => {
              if (!peer.closed && typeof peer.onmessage === 'function') {
                peer.onmessage({ data });
              }
            }, 0);
          }
        },
        close() {
          this.closed = true;
          peers.delete(channel);
        },
      };
      peers.add(channel);
      return channel;
    },
  };
}

const ENDPOINT = 'https://hyoui.example.jp/';

/// 1 タブ分。`refreshes` は「そのタブが実際に server を叩いた回数」。
function createTab(options) {
  const state = { refreshes: 0 };
  const share = createAuthShare({
    endpoint: options.endpoint ?? ENDPOINT,
    sub: options.sub ?? null,
    locks: options.locks ?? null,
    makeChannel: options.hub ? (name) => options.hub.make(name) : null,
    now: options.now ?? (() => Date.now()),
    askTimeoutMs: options.askTimeoutMs ?? 5,
    refresh: async () => {
      state.refreshes += 1;
      return options.serve(state.refreshes);
    },
  });
  return { share, state };
}

test('2 つのタブの access が同時に切れても refresh は 1 回だけ走る', async () => {
  const locks = createLockManager();
  const hub = createChannelHub();
  let clock = 1_000;
  const now = () => clock;
  // server 側の据え置き: 誰が聞いても同じ値を返す (決定 5 のサーバ手順)。
  const serve = () => ({ accessToken: 'access-1', expiresAtMs: clock + 10_000, sub: 'sub-1' });

  const first = createTab({ locks, hub, now, serve, sub: 'sub-1' });
  const second = createTab({ locks, hub, now, serve, sub: 'sub-1' });

  const [a, b] = await Promise.all([first.share.ensure(), second.share.ensure()]);

  assert.equal(a.value, 'access-1');
  assert.equal(b.value, 'access-1');
  assert.equal(
    first.state.refreshes + second.state.refreshes,
    1,
    '同時に切れても server を叩くのは 1 タブだけ',
  );
});

test('refresh したタブの結果が、待っていたタブにも届く', async () => {
  const locks = createLockManager();
  const hub = createChannelHub();
  const now = () => 1_000;
  const serve = () => ({ accessToken: 'access-shared', expiresAtMs: 11_000, sub: 'sub-1' });

  const refresher = createTab({ locks, hub, now, serve, sub: 'sub-1' });
  const waiter = createTab({ locks, hub, now, serve, sub: 'sub-1' });

  const received = [];
  waiter.share.onAccess((access) => received.push(access.value));

  await refresher.share.ensure();
  // message は非同期に届く。
  await new Promise((resolve) => setTimeout(resolve, 5));

  assert.deepEqual(received, ['access-shared'], '配られた値がそのまま届く');
  assert.equal(waiter.share.current().value, 'access-shared');
  assert.equal(waiter.state.refreshes, 0, '待っていた側は server を叩かない');
});

test('期限切れの値は配らない (受けた側も提示に使わない)', async () => {
  const hub = createChannelHub();
  let clock = 1_000;
  const now = () => clock;

  const receiver = createTab({
    hub,
    now,
    sub: 'sub-1',
    serve: () => ({ accessToken: 'own', expiresAtMs: clock + 10_000, sub: 'sub-1' }),
  });

  // 既に切れている値を「持っている」と名乗る相手を直に作る。
  const rogue = hub.make(`hyoui.auth:${ENDPOINT}`);
  rogue.postMessage({ kind: 'offer', sub: 'sub-1', accessToken: 'stale', expiresAtMs: clock - 1 });
  await new Promise((resolve) => setTimeout(resolve, 5));

  assert.equal(receiver.share.current(), null, '期限切れの offer は採らない');

  // 自分が持っている値が切れたら、それも配らない (= ask に答えない)。
  await receiver.share.ensure();
  assert.equal(receiver.share.current().value, 'own');
  clock += 20_000;
  assert.equal(receiver.share.current(), null, '切れた値は current でも返さない');

  const asker = hub.make(`hyoui.auth:${ENDPOINT}`);
  const answers = [];
  asker.onmessage = (event) => {
    if (event.data.kind === 'offer') answers.push(event.data.accessToken);
  };
  asker.postMessage({ kind: 'ask' });
  await new Promise((resolve) => setTimeout(resolve, 5));
  assert.deepEqual(answers, [], '切れた値は配らない');
});

test('開いたばかりのタブは、問い合わせで既に立っている値を受け取る', async () => {
  const locks = createLockManager();
  const hub = createChannelHub();
  const now = () => 1_000;
  const serve = (count) => ({
    accessToken: `access-${count}`,
    expiresAtMs: 11_000,
    sub: 'sub-1',
  });

  const established = createTab({ locks, hub, now, serve, sub: 'sub-1' });
  await established.share.ensure();

  // **後から開いたタブは、自分が存在する前に配られたものを受け取れない。**
  // listen しているだけでは永久に届かないので、問い合わせる側に回る (reference 手順 3)。
  const opened = createTab({ locks, hub, now, serve, sub: 'sub-1' });
  const access = await opened.share.ensure();

  assert.equal(access.value, 'access-1', '立っている値をそのまま使う');
  assert.equal(opened.state.refreshes, 0, '問い合わせで足りたので refresh しない');
});

test('誰も答えなければ自分で refresh する', async () => {
  const locks = createLockManager();
  const hub = createChannelHub();
  const now = () => 1_000;

  const lonely = createTab({
    locks,
    hub,
    now,
    sub: 'sub-1',
    serve: () => ({ accessToken: 'mine', expiresAtMs: 11_000, sub: 'sub-1' }),
  });

  const access = await lonely.share.ensure();
  assert.equal(access.value, 'mine');
  assert.equal(lonely.state.refreshes, 1, '打ち切りの後に自分で取る');
});

test('ロックが無い環境では各タブが自分で refresh する', async () => {
  const hub = createChannelHub();
  const now = () => 1_000;
  let issued = 0;
  // ロックが無いので両方が server を叩く。収束はサーバ側の据え置きが担う
  // (= 同じ値を返す)。
  const serve = () => {
    issued += 1;
    return { accessToken: 'standing', expiresAtMs: 11_000, sub: 'sub-1' };
  };

  const first = createTab({ hub, now, serve, sub: 'sub-1', locks: null });
  const second = createTab({ hub, now, serve, sub: 'sub-1', locks: null });

  const [a, b] = await Promise.all([first.share.ensure(), second.share.ensure()]);

  assert.equal(a.value, 'standing');
  assert.equal(b.value, 'standing');
  assert.equal(issued, 2, 'ロックが無ければ各タブが自分で取る (= 協調は前提ではない)');
});

test('endpoint が違うタブ、sub が違うタブとは調停しない', async () => {
  const locks = createLockManager();
  const hub = createChannelHub();
  const now = () => 1_000;

  const mine = createTab({
    locks,
    hub,
    now,
    sub: 'sub-1',
    serve: () => ({ accessToken: 'mine', expiresAtMs: 11_000, sub: 'sub-1' }),
  });
  const otherSub = createTab({
    locks,
    hub,
    now,
    sub: 'sub-2',
    serve: () => ({ accessToken: 'theirs', expiresAtMs: 11_000, sub: 'sub-2' }),
  });
  const otherEndpoint = createTab({
    locks,
    hub,
    now,
    endpoint: 'https://hyoui-unstable.example.jp/',
    sub: 'sub-1',
    serve: () => ({ accessToken: 'elsewhere', expiresAtMs: 11_000, sub: 'sub-1' }),
  });

  // **refresh の排他は sub ごとに分かれる** (= 無関係なタブ同士が待ち合わない)。
  assert.notEqual(mine.share.names().lock, otherSub.share.names().lock);
  assert.notEqual(mine.share.names().lock, otherEndpoint.share.names().lock);
  // channel は endpoint 単位で 1 本 (= 開いたばかりのタブが届く先が要る)。
  // sub が違うタブは同じ channel に居るが、offer は message の sub で捨てる。
  assert.equal(mine.share.names().channel, otherSub.share.names().channel);
  assert.notEqual(mine.share.names().channel, otherEndpoint.share.names().channel);

  await Promise.all([
    mine.share.ensure(),
    otherSub.share.ensure(),
    otherEndpoint.share.ensure(),
  ]);
  await new Promise((resolve) => setTimeout(resolve, 5));

  // 3 者とも自分で取っている (= 待ち合わせていない)。
  assert.equal(mine.state.refreshes, 1);
  assert.equal(otherSub.state.refreshes, 1);
  assert.equal(otherEndpoint.state.refreshes, 1);
  // 値が混ざらない。
  assert.equal(mine.share.current().value, 'mine');
  assert.equal(otherSub.share.current().value, 'theirs');
  assert.equal(otherEndpoint.share.current().value, 'elsewhere');
});

// ---- 上の 7 性質の土台になる 2 点 ----

test('sub が分かる前は endpoint だけの key で待ち、確定後に張り替える', async () => {
  // reference 手順 5。ログイン前のタブは自分の sub を知らない。
  const hub = createChannelHub();
  const anonymous = createTab({
    hub,
    now: () => 1_000,
    sub: null,
    serve: () => ({ accessToken: 'after-login', expiresAtMs: 11_000, sub: 'sub-9' }),
  });
  assert.equal(anonymous.share.names().lock, `hyoui.auth.refresh:${ENDPOINT}`);
  assert.equal(anonymous.share.names().channel, `hyoui.auth:${ENDPOINT}`);

  await anonymous.share.ensure();
  assert.equal(anonymous.share.sub(), 'sub-9', 'refresh の答えで sub が確定する');
  assert.equal(
    anonymous.share.names().lock,
    `hyoui.auth.refresh:${ENDPOINT}:sub-9`,
    '確定したら排他の名前を張り替える',
  );
  assert.equal(
    anonymous.share.names().channel,
    `hyoui.auth:${ENDPOINT}`,
    '待ち合わせ場所は endpoint 単位のまま (= 後から開くタブが届く先)',
  );
});

test('拒否された値は捨てて取り直す (backoff ループに入らない)', async () => {
  // reference 手順 4 の順序。同じ値の再提示を繰り返さない。
  const locks = createLockManager();
  const hub = createChannelHub();
  let clock = 1_000;
  const tab = createTab({
    locks,
    hub,
    now: () => clock,
    sub: 'sub-1',
    serve: (count) => ({
      accessToken: `access-${count}`,
      expiresAtMs: clock + 10_000,
      sub: 'sub-1',
    }),
  });

  const first = await tab.share.ensure();
  assert.equal(first.value, 'access-1');

  // server が拒否した = ローカルの期限は当てにならない。捨てて取り直す。
  const second = await tab.share.renew('access-1');
  assert.equal(second.value, 'access-2');
  assert.equal(tab.state.refreshes, 2);
});

test('sub を知らないタブが、sub を知っているタブから立っている値を受け取る', async () => {
  // **実機で踏んだ形** (playwright の 2 タブ、2026-09-16): channel も sub で
  // 張り替えると、開いたばかりのタブ (sub 未知) は sub を知っているタブに ask が
  // 届かず、共有できずに refresh してしまう (= server 側で rotate が 1 増える)。
  // 待ち合わせ場所を endpoint 単位にしてあることをこの test で固定する。
  const locks = createLockManager();
  const hub = createChannelHub();
  const now = () => 1_000;

  const known = createTab({
    locks,
    hub,
    now,
    sub: null,
    serve: () => ({ accessToken: 'standing', expiresAtMs: 11_000, sub: 'sub-7' }),
  });
  await known.share.ensure();
  assert.equal(known.share.sub(), 'sub-7', 'ログインで sub が確定した側');

  // 後から開いたタブは自分の sub を知らない。
  const newcomer = createTab({
    locks,
    hub,
    now,
    sub: null,
    serve: () => ({ accessToken: 'should-not-refresh', expiresAtMs: 11_000, sub: 'sub-7' }),
  });
  const access = await newcomer.share.ensure();

  assert.equal(access.value, 'standing', '立っている値をそのまま受け取る');
  assert.equal(newcomer.state.refreshes, 0, 'server を叩かない (= rotate が増えない)');
  assert.equal(newcomer.share.sub(), 'sub-7', 'offer に載った sub で自分の sub が決まる');
});

test('期限が近づいたら、立っている値があっても取り直す', async () => {
  // **実機で踏んだ形** (playwright + CDP の WS frame 観測、2026-09-16): 予定した
  // 延長 (残り 10% 時点、決定 5) を `ensure` で呼ぶと「まだ期限内」で何も起きず、
  // 期限まで放置されて接続が切れる。先回りの延長は別の入口が要る。
  const locks = createLockManager();
  const hub = createChannelHub();
  const now = () => 1_000;
  const tab = createTab({
    locks,
    hub,
    now,
    sub: 'sub-1',
    serve: (count) => ({ accessToken: `access-${count}`, expiresAtMs: 11_000, sub: 'sub-1' }),
  });

  await tab.share.ensure();
  assert.equal(tab.state.refreshes, 1);

  // 期限内なので `ensure` は何もしない。
  await tab.share.ensure();
  assert.equal(tab.state.refreshes, 1, 'ensure は期限内なら server を叩かない');

  // 先回りの延長は、期限内でも取り直す。
  const ahead = await tab.share.refreshAhead();
  assert.equal(ahead.value, 'access-2');
  assert.equal(tab.state.refreshes, 2, 'refreshAhead は期限内でも取り直す');
});

test('先回りの延長でも、他のタブが持っている値があれば server を叩かない', async () => {
  const locks = createLockManager();
  const hub = createChannelHub();
  const now = () => 1_000;
  const serve = (count) => ({ accessToken: `a-${count}`, expiresAtMs: 11_000, sub: 'sub-1' });

  const holder = createTab({ locks, hub, now, serve, sub: 'sub-1' });
  const other = createTab({ locks, hub, now, serve, sub: 'sub-1' });
  await holder.share.ensure();
  await new Promise((resolve) => setTimeout(resolve, 5));

  await other.share.refreshAhead();
  assert.equal(other.state.refreshes, 0, '協調の中で取り直すので往復が増えない');
  assert.equal(other.share.current().value, 'a-1');
});
