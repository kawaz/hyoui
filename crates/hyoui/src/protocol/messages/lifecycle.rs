//! `detach` / `kill` payload (DR-0008 §2.3)。

use serde::{Deserialize, Serialize};

/// detach 対象 (DR-0006 §3-4)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum DetachTarget {
    /// 自分のみ切断 (default)。
    #[serde(rename = "self")]
    Myself,
    /// 自分以外の全 client を daemon が切断、自分は残る。
    Others,
    /// 自分含む全 client を切断 (= daemon は子 PTY 接続維持、新規 attach 待ち)。
    All,
}

/// `detach` payload。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Detach {
    /// 切断対象。
    pub target: DetachTarget,
}

/// `kill` payload (DR-0012)。daemon は子 PTY に signal を送って自身も exit する。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Kill {
    /// 子に送る signal 名 (= null なら SIGTERM、正規表記は SIG-prefix 大文字)。
    /// 未知 name は daemon 側で `signal.invalid` で reject される。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,

    /// client が「子 exit + session 終了まで見届けて返る」ことを要求するか
    /// (= `hyoui kill --wait`)。
    ///
    /// - `false` (default): daemon は signal 受理時点で `KillAck` を 1 frame
    ///   返す (= client は即時 return)。terminate (= waitpid → reap) は daemon
    ///   内で非同期に進行する。`kill(1)` と同じ直感 (= 子が 1 発で死なない app
    ///   でも無応答にならない)。
    /// - `true`: daemon は `KillAck` を送らず、従来通り session terminate 完了
    ///   (= socket EOF) まで socket を開いたまま保つ。client は EOF を待つ。
    ///
    /// 旧 client (= field 無し) からの Kill は `#[serde(default)]` で `false`
    /// (= 即時 ack) になる。旧 client は KillAck を未知 kind として decode error
    /// → EOF 相当に倒れて成功扱いになる (= 互換)。
    ///
    /// 注: `hyoui kill --wait` (timeout 付き見届け) は **`wait: false` を送る**。
    /// client が `KillAck` 後も接続を維持し、`session.exit.notify` / EOF を自前
    /// deadline 付きで待つ (= timeout 時は session を畳まず exit 3、daemon は
    /// 一切 block しないので孤児化しない)。`wait: true` は legacy client 互換用で、
    /// daemon 側は terminate 経路 (= `finalize_child` の bounded escalation) で応じる。
    #[serde(default)]
    pub wait: bool,
}

/// `kill.ack` payload (= 即時応答 kill)。daemon が `Kill { wait: false }` を
/// 受理し、子に signal を送った直後に返す ack。
///
/// この frame を受け取った client は「signal 送信依頼が受理された」と判断して
/// 即時 return する (= 子の実 exit は待たない)。terminate は daemon 内で非同期
/// 進行する。`wait: true` の Kill にはこの ack は返らない (= 従来通り EOF 待ち)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct KillAck {
    /// daemon が子に送った signal 名 (= 正規表記 SIG-prefix 大文字)。
    /// client の情報表示用 (= 「SIGTERM 送信完了」等)。
    pub signal: String,
}
