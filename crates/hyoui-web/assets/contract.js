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

  window.hyouiContract = { WEB_PROTOCOL_VERSION, httpError, frameErrorText };
})();
