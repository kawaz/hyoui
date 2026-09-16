// hyoui web — browser 側が持つ契約 (DR-0035)。
//
// 契約の正本は Rust の `crates/hyoui-web/src/contract.rs`。ここに置くのは
// 「JS 側に無いと成立しないもの」だけで、kind 一覧や cap 集合の写しは持たない
// (DR-0035「増やしたくないもの」)。
(() => {
  // ---- 契約の世代番号 (DR-0035 決定 3) ----
  //
  // Rust 側 `contract.rs` の `WEB_PROTOCOL_VERSION` と同じ値を持つ。一致は Rust の
  // test が この行を正規表現で読んで固定する。**JS が持つ定数はこれ 1 つだけ** で、
  // kind 一覧や cap 集合の写しは持たない。
  const WEB_PROTOCOL_VERSION = 1;

  // ---- エラー形 (DR-0035 決定 2) ----
  //
  // HTTP も WS も `{"error": {"code", "message"}}` の 1 型。plain text body の
  // 経路は無いので、fallback は「読めなかった」を表すためだけに置く。

  function errorText(error, fallback) {
    if (!error || typeof error !== 'object') return fallback;
    const message = typeof error.message === 'string' ? error.message : '';
    const code = typeof error.code === 'string' ? error.code : '';
    if (message && code) return `${message} (${code})`;
    return message || code || fallback;
  }

  // 失敗した Response を Error に変換する。body が読めない場合も status は残す。
  async function httpError(response) {
    const fallback = `HTTP ${response.status}`;
    let body = null;
    try {
      body = await response.json();
    } catch (_e) {
      return new Error(fallback);
    }
    return new Error(`${fallback}: ${errorText(body && body.error, 'unreadable error body')}`);
  }

  // `*.result` / `error` frame の error field を人向け文字列にする。
  function frameErrorText(frame, fallback) {
    return errorText(frame && frame.error, fallback);
  }

  // ---- 世代不一致の検出と帯 (DR-0035 決定 3 / 決定 5) ----
  //
  // 平常時、assets は gateway binary に埋め込まれて配られるので配る側と受ける側は
  // 同じビルドである。ずれるのは「ページを開いたまま gateway が入れ替わった」場合と
  // 「HA endpoint の裏で stable / unstable が入れ替わった」場合の 2 つ。
  //
  // 検出したら出すのは「再読み込みしてください」の 1 つだけで、互換経路は持たない。
  // 自動リロードはしない (kawaz 指示 — 入力中の内容を予告なく捨てる)。

  let mismatch = null;

  // gateway 世代を人向けに書く。protocol を名乗らない gateway は「不明」。
  function describeGatewayProtocol(value) {
    return typeof value === 'number' ? String(value) : '不明 (protocol を名乗らない版)';
  }

  function showMismatchBanner() {
    if (document.getElementById('protocolBanner')) return;
    const banner = document.createElement('div');
    banner.id = 'protocolBanner';
    banner.className = 'protocol-banner';
    banner.setAttribute('role', 'alert');
    const text = document.createElement('span');
    text.textContent = `この画面は契約世代 ${mismatch.page}、gateway は ${describeGatewayProtocol(mismatch.gateway)} です。再読み込みしてください。`;
    const button = document.createElement('button');
    button.type = 'button';
    button.textContent = '再読み込み';
    // query (表示設定) をそのまま残すため location.reload() のみ。
    button.addEventListener('click', () => location.reload());
    banner.append(text, button);
    document.body.insertBefore(banner, document.body.firstChild);
  }

  // gateway が名乗った世代を受け取る。不一致なら帯を出して true を返す。
  //
  // `hello` は再接続のたびに届くので、fallback で裏の unit が変わっても拾える。
  // index ページは `/version` の `protocol` を同じ周期で渡す。
  function reportGatewayProtocol(gatewayProtocol) {
    if (gatewayProtocol === WEB_PROTOCOL_VERSION) return false;
    if (!mismatch) {
      mismatch = { page: WEB_PROTOCOL_VERSION, gateway: gatewayProtocol };
      if (document.body) showMismatchBanner();
      else window.addEventListener('DOMContentLoaded', showMismatchBanner);
    }
    return true;
  }

  // 世代が合っていないと分かっているか。制御 frame の送信可否に使う。
  //
  // 一度検出したら戻さない: 同じページの JS が新しい gateway の契約を話せる
  // ようになることは無く、収束させる手段は reload だけである。
  function protocolMismatched() {
    return mismatch !== null;
  }

  window.hyouiContract = {
    WEB_PROTOCOL_VERSION,
    httpError,
    frameErrorText,
    reportGatewayProtocol,
    protocolMismatched,
  };
})();
