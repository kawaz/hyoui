//! `auth.json` / `pending.json` に載る形 (DR-0036 決定 4 / 決定 5)。
//!
//! **endpoint は「引く単位」そのもの**で、record の属性ではなく key の 1 段である
//! (決定 4)。stable の unit に別 endpoint の credential が提示されることは前段の
//! 構成上起きないが、起きても record の endpoint との照合で落ちるだけで、unit 側に
//! 「自分の endpoint」の知識は要らない。
//!
//! 時刻はすべて unix epoch の ms (`u64`) で持つ。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::contract::Endpoint;

/// credential の claim (DR-0036 決定 7)。
///
/// **当面は全ての登録が `Rw` で、gateway はこれを読んで分岐しない。**
/// 今 field を定義しておくのは、後から足すと「既に登録された credential に
/// `access` が無い状態」を扱う分岐が要るためである。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Access {
    /// 読み書き。現状はこれだけが使われる。
    #[default]
    #[serde(rename = "rw")]
    Rw,
    /// 読み取り専用。**実装は持たない** (決定 7 の拡張点)。
    #[serde(rename = "ro")]
    Ro,
}

impl Access {
    /// `passkey list` に出す表記。
    pub fn as_str(&self) -> &'static str {
        match self {
            Access::Rw => "rw",
            Access::Ro => "ro",
        }
    }
}

// -----------------------------------------------------------------------------
// auth.json
// -----------------------------------------------------------------------------

/// `auth.json` の中身 (決定 4)。
///
/// key は `family/<endpoint>/<id>` で、endpoint は record の属性ではなく key の 1 段。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthFile {
    /// token family。`family/<endpoint>/<id>`。
    #[serde(default)]
    pub families: BTreeMap<Endpoint, BTreeMap<String, FamilyRecord>>,
}

/// access / refresh の 1 世代 (決定 5)。
///
/// token は署名しない opaque 乱数で、検証は record の lookup で行う。署名鍵を持つと
/// 保管・rotate・配布という管理対象が増えるが、record を引く形なら鍵なしで済む。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenGeneration {
    /// 値 (base64url)。access は **WS subprotocol に載せるため** base64url である
    /// (`Sec-WebSocket-Protocol` は RFC 6455 の token でなければならず、素の base64 の
    /// `+` `/` `=` は使えない)。
    pub value: String,
    /// 失効時刻 (unix ms)。
    pub expires_at_ms: u64,
}

impl TokenGeneration {
    /// `now_ms` 時点で生きているか。
    pub fn is_live(&self, now_ms: u64) -> bool {
        now_ms < self.expires_at_ms
    }

    /// 残り寿命が `ttl_ms` の半分以上あるか (決定 5 の「access は据え置く」)。
    ///
    /// 据え置きが複数タブで 1 本の access を共有する土台になる
    /// (reference `multi-tab-token-refresh` のサーバ側手順)。
    pub fn has_half_life_left(&self, now_ms: u64, ttl_ms: u64) -> bool {
        self.expires_at_ms.saturating_sub(now_ms) * 2 >= ttl_ms
    }
}

/// 退役した refresh の 1 世代 (決定 5)。
///
/// 値そのものは持たず**ダイジェストを本来の exp まで保持**する。どの世代の値でも
/// 再提示を見たら family ごと失効させる。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetiredRefresh {
    /// 退役した値の sha256 (hex)。
    pub digest: String,
    /// その値本来の失効時刻 (unix ms)。ここを過ぎたら掃除してよい。
    pub expires_at_ms: u64,
    /// 退役した時刻 (unix ms)。**直前 1 世代の再送猶予**の判定に使う。
    pub retired_at_ms: u64,
}

/// token family (決定 5)。
///
/// `/auth/refresh` は cookie の値で family を引き、**その family の `endpoint` が
/// 要求 body の `endpoint` と一致することを確認する**。cookie が endpoint ごとに
/// 分かれていても、確認は値の側で行う。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FamilyRecord {
    /// family の id。
    pub id: String,
    /// 誰の family か (= credential の `sub`)。
    pub sub: String,
    /// どの endpoint で mint したか。**key と重複するが、cookie 由来の lookup では
    /// key を知らずに引くので record 側にも要る** (決定 5)。
    pub endpoint: Endpoint,
    /// 現行の access。
    pub access: TokenGeneration,
    /// 現行の refresh。
    pub refresh: TokenGeneration,
    /// 退役世代。新しいものが末尾。
    #[serde(default)]
    pub retired: Vec<RetiredRefresh>,
    /// 失効時刻 (unix ms)。`Some` なら tombstone (決定 4 — 削除ではない)。
    #[serde(default)]
    pub tombstoned_at_ms: Option<u64>,
}

/// 直前 1 世代の再送猶予 (決定 5)。
pub const REFRESH_REPLAY_GRACE_MS: u64 = 60_000;

/// refresh token の寿命 (7 日、決定 5)。
pub const REFRESH_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// access token の寿命 (4 時間、決定 5)。
///
/// **失効が確立済みの WS に効くまでの最長の猶予がこの値になる** (決定 4)。
pub const ACCESS_TTL_MS: u64 = 4 * 60 * 60 * 1000;

/// 提示された refresh 値を family に照らした結果 (決定 5)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// 現行世代の提示。rotate してよい。
    Current,
    /// 直前 1 世代の猶予内の再送。**rotate せず前回の答えを返す。**
    Replay,
    /// どの世代かの再提示 (猶予外)。**family ごと失効させる。**
    Reused,
    /// この family の値ではない。
    Unknown,
}

impl FamilyRecord {
    /// tombstone 済みか。
    pub fn is_tombstoned(&self) -> bool {
        self.tombstoned_at_ms.is_some()
    }

    /// 提示された refresh 値をこの family に照らす。
    ///
    /// 比較は**タイミング安全**に行う (決定 5 が ccmsg から借りる実装判断)。
    pub fn classify_refresh(&self, presented: &str, now_ms: u64) -> RefreshOutcome {
        if constant_time_eq(self.refresh.value.as_bytes(), presented.as_bytes()) {
            return RefreshOutcome::Current;
        }
        let digest = sha256_hex(presented.as_bytes());
        // 末尾が直前世代。猶予内ならこれだけが Replay になる。
        if let Some(previous) = self.retired.last()
            && constant_time_eq(previous.digest.as_bytes(), digest.as_bytes())
            && now_ms.saturating_sub(previous.retired_at_ms) <= REFRESH_REPLAY_GRACE_MS
        {
            return RefreshOutcome::Replay;
        }
        let seen = self
            .retired
            .iter()
            .any(|retired| constant_time_eq(retired.digest.as_bytes(), digest.as_bytes()));
        if seen {
            RefreshOutcome::Reused
        } else {
            RefreshOutcome::Unknown
        }
    }

    /// refresh を rotate する。現行世代は退役世代に落ちる。
    ///
    /// access は据え置く: **残り寿命が TTL の半分以上あれば差し替えない** (決定 5)。
    /// これがクライアントのロックが取りこぼした分の保険になる。
    pub fn rotate_refresh(
        &mut self,
        new_refresh: String,
        new_access: impl FnOnce() -> String,
        now_ms: u64,
    ) {
        self.retired.push(RetiredRefresh {
            digest: sha256_hex(self.refresh.value.as_bytes()),
            expires_at_ms: self.refresh.expires_at_ms,
            retired_at_ms: now_ms,
        });
        // 本来の exp を過ぎた退役世代は掃除する (= 掃除は timer でなく読み書き時、決定 5)。
        self.retired
            .retain(|retired| now_ms < retired.expires_at_ms);
        self.refresh = TokenGeneration {
            value: new_refresh,
            expires_at_ms: now_ms + REFRESH_TTL_MS,
        };
        if !self.access.is_live(now_ms) || !self.access.has_half_life_left(now_ms, ACCESS_TTL_MS) {
            self.access = TokenGeneration {
                value: new_access(),
                expires_at_ms: now_ms + ACCESS_TTL_MS,
            };
        }
    }
}

// -----------------------------------------------------------------------------
// pending.json
// -----------------------------------------------------------------------------

/// `pending.json` の中身 (決定 4)。
///
/// **CLI が直接書き、gateway は読むだけである** (決定 2)。gateway に管理用の経路を
/// 足さずに済み、`hyoui web daemon` が全て停止していても `passkey add` が打てる。
/// secret がプロセスのメモリに無いので、どの unit が POST を受けても検証できる。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PendingFile {
    /// 登録 1 本。`<endpoint>` → `jti`。
    #[serde(default)]
    pub registrations: BTreeMap<Endpoint, BTreeMap<String, PendingRegistration>>,
    /// challenge の在庫。`<endpoint>` → challenge id。
    #[serde(default)]
    pub challenges: BTreeMap<Endpoint, BTreeMap<String, PendingChallenge>>,
}

/// 登録 URL の寿命 (10 分、決定 2)。
pub const REGISTRATION_TTL_MS: u64 = 10 * 60 * 1000;

/// 6 桁コードの試行上限 (決定 2)。到達でその jti を焼く。
pub const CODE_ATTEMPT_LIMIT: u32 = 5;

/// CLI が発行した登録 1 本 (決定 2)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingRegistration {
    /// jwt の `jti`。
    pub jti: String,
    /// 登録先の `sub`。
    pub sub: String,
    /// user handle (16 byte)。**sub ごとに 1 度だけ決めて使い回す** — 同じ人に 2 つの
    /// handle を配ると端末上で 2 つのアカウントに見える (決定 3)。
    pub user_id: Vec<u8>,
    /// claim (決定 7)。
    #[serde(default)]
    pub access: Access,
    /// jwt の署名に使う乱数 32 byte の HMAC secret。**永続鍵を持たない** (決定 2)。
    pub hmac_secret: Vec<u8>,
    /// 6 桁コードの sha256 (hex)。コード自体は CLI にだけ表示する。
    pub code_hash: String,
    /// 誤入力の回数。`CODE_ATTEMPT_LIMIT` でこの jti を焼く。
    #[serde(default)]
    pub code_attempts: u32,
    /// 発行時に管理者が付けたラベル (`--label`)。**認証の材料ではない** (決定 3)。
    #[serde(default)]
    pub issued_label: Option<String>,
    /// 失効時刻 (unix ms)。
    pub expires_at_ms: u64,
}

/// 6 桁コードの照合結果 (決定 2)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeOutcome {
    /// 一致。
    Accepted,
    /// 不一致。まだ試行できる。
    Rejected {
        /// 残り試行回数。**失敗理由を URL とコードで分けない**ので、呼び出し側は
        /// これを応答に載せない (決定 5)。log と CLI の観測にだけ使う。
        remaining: u32,
    },
    /// 不一致で上限に到達した。この jti は焼ける。
    Burned,
}

impl PendingRegistration {
    /// `now_ms` 時点で生きているか。
    pub fn is_live(&self, now_ms: u64) -> bool {
        now_ms < self.expires_at_ms
    }

    /// 6 桁コードを照合し、**外れたら試行回数を加算する**。
    ///
    /// 呼び出し側は必ず `PendingFile` の lock 下で使う (決定 4) — lock の外で数えると
    /// 2 unit に来た要求で数え落とし、総当たりの回数上限が unit の数だけ緩む。
    pub fn check_code(&mut self, presented: &str) -> CodeOutcome {
        let digest = sha256_hex(presented.as_bytes());
        if constant_time_eq(self.code_hash.as_bytes(), digest.as_bytes()) {
            return CodeOutcome::Accepted;
        }
        self.code_attempts = self.code_attempts.saturating_add(1);
        if self.code_attempts >= CODE_ATTEMPT_LIMIT {
            CodeOutcome::Burned
        } else {
            CodeOutcome::Rejected {
                remaining: CODE_ATTEMPT_LIMIT - self.code_attempts,
            }
        }
    }
}

/// 発行済みの challenge 1 本 (決定 3 / 決定 4)。
///
/// **challenge にも endpoint を埋める。** 「challenge を取った endpoint」と「使う
/// endpoint」のすり替えが効かないようにするため (決定 3)。key の endpoint と
/// 重複するが、消費時に record 側の値で照合する。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingChallenge {
    /// challenge の識別子。
    pub id: String,
    /// この challenge を発行した endpoint。
    pub endpoint: Endpoint,
    /// 用途。登録用の challenge を認証に使い回させない。
    pub purpose: ChallengePurpose,
    /// 失効時刻 (unix ms)。
    pub expires_at_ms: u64,
}

/// challenge の用途。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChallengePurpose {
    /// `navigator.credentials.create()` 用。
    #[serde(rename = "register")]
    Register,
    /// `navigator.credentials.get()` 用。
    #[serde(rename = "assert")]
    Assert,
}

impl PendingFile {
    /// 期限切れを落とす。**掃除は timer でなく読み取り時に行う** (決定 5)。
    pub fn sweep_expired(&mut self, now_ms: u64) {
        for registrations in self.registrations.values_mut() {
            registrations.retain(|_, registration| registration.is_live(now_ms));
        }
        self.registrations
            .retain(|_, registrations| !registrations.is_empty());
        for challenges in self.challenges.values_mut() {
            challenges.retain(|_, challenge| now_ms < challenge.expires_at_ms);
        }
        self.challenges
            .retain(|_, challenges| !challenges.is_empty());
    }

    /// challenge を 1 本消費する (= 使用済みにする)。既に無ければ `None`。
    ///
    /// **lock 下で呼ぶ。** lock の外で消費すると、2 unit に同時に来た要求が同じ
    /// challenge を 2 回消費できる (決定 4)。
    pub fn consume_challenge(
        &mut self,
        endpoint: &Endpoint,
        id: &str,
        purpose: ChallengePurpose,
        now_ms: u64,
    ) -> Option<PendingChallenge> {
        let challenges = self.challenges.get_mut(endpoint)?;
        let challenge = challenges.get(id)?;
        // 用途違い / 期限切れ / endpoint 不一致はいずれも「無い」と同じに扱う
        // (= 失敗理由を分けない、決定 5)。
        if challenge.purpose != purpose
            || now_ms >= challenge.expires_at_ms
            || &challenge.endpoint != endpoint
        {
            return None;
        }
        challenges.remove(id)
    }
}

// -----------------------------------------------------------------------------
// 比較とダイジェスト
// -----------------------------------------------------------------------------

/// タイミング安全な byte 列比較 (決定 5 が ccmsg から借りる実装判断)。
///
/// challenge / token / 6 桁コードの比較に使う。長さの違いは隠さない (= 長さは
/// 秘密ではない) が、内容の一致位置は漏らさない。
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// sha256 の hex 表記。
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint() -> Endpoint {
        Endpoint::parse("https://hyoui.example.jp/").unwrap()
    }

    fn family(now_ms: u64) -> FamilyRecord {
        FamilyRecord {
            id: "fam1".to_string(),
            sub: "hyoui.example.jp-1".to_string(),
            endpoint: endpoint(),
            access: TokenGeneration {
                value: "access-1".to_string(),
                expires_at_ms: now_ms + ACCESS_TTL_MS,
            },
            refresh: TokenGeneration {
                value: "refresh-1".to_string(),
                expires_at_ms: now_ms + REFRESH_TTL_MS,
            },
            retired: Vec::new(),
            tombstoned_at_ms: None,
        }
    }

    #[test]
    fn current_generation_is_accepted_and_older_ones_are_reuse() {
        let now = 1_000_000;
        let mut record = family(now);
        assert_eq!(
            record.classify_refresh("refresh-1", now),
            RefreshOutcome::Current
        );
        assert_eq!(
            record.classify_refresh("nope", now),
            RefreshOutcome::Unknown,
            "この family の値でなければ Unknown (= 別 family の値を再利用扱いにしない)"
        );

        record.rotate_refresh("refresh-2".to_string(), || "access-2".to_string(), now);
        assert_eq!(
            record.classify_refresh("refresh-2", now),
            RefreshOutcome::Current
        );
        // 直前世代は猶予内なら Replay (= rotate せず前回の答えを返す)。
        assert_eq!(
            record.classify_refresh("refresh-1", now + 1_000),
            RefreshOutcome::Replay
        );
        // 猶予を過ぎた再提示は family ごと失効させる。
        assert_eq!(
            record.classify_refresh("refresh-1", now + REFRESH_REPLAY_GRACE_MS + 1),
            RefreshOutcome::Reused
        );
    }

    #[test]
    fn two_generations_back_is_reuse_even_inside_the_grace_window() {
        // 猶予は「直前 1 世代」だけ。2 世代前は猶予内でも再利用である。
        let now = 1_000_000;
        let mut record = family(now);
        record.rotate_refresh("refresh-2".to_string(), || "access-2".to_string(), now);
        record.rotate_refresh("refresh-3".to_string(), || "access-3".to_string(), now + 10);
        assert_eq!(
            record.classify_refresh("refresh-1", now + 20),
            RefreshOutcome::Reused
        );
        assert_eq!(
            record.classify_refresh("refresh-2", now + 20),
            RefreshOutcome::Replay
        );
    }

    #[test]
    fn access_is_held_until_half_its_life_is_gone() {
        // reference `multi-tab-token-refresh` のサーバ側手順。据え置きが
        // 複数タブで 1 本の access を共有する土台になる。
        let now = 1_000_000;
        let mut record = family(now);
        record.rotate_refresh("refresh-2".to_string(), || "access-new".to_string(), now);
        assert_eq!(
            record.access.value, "access-1",
            "残り寿命が半分以上なら据え置く"
        );

        let later = now + ACCESS_TTL_MS / 2 + 1;
        record.rotate_refresh("refresh-3".to_string(), || "access-new".to_string(), later);
        assert_eq!(
            record.access.value, "access-new",
            "半分を切って初めて mint し直す"
        );
        assert_eq!(record.access.expires_at_ms, later + ACCESS_TTL_MS);
    }

    #[test]
    fn retired_generations_are_swept_at_their_own_expiry() {
        // 掃除は timer でなく読み書き時に行う (決定 5)。
        let now = 1_000_000;
        let mut record = family(now);
        record.rotate_refresh("refresh-2".to_string(), || "a".to_string(), now);
        assert_eq!(record.retired.len(), 1);
        // 退役世代の本来の exp を過ぎた時点の rotate で落ちる。
        record.rotate_refresh(
            "refresh-3".to_string(),
            || "a".to_string(),
            now + REFRESH_TTL_MS + 1,
        );
        assert!(
            record
                .retired
                .iter()
                .all(|retired| retired.digest != sha256_hex(b"refresh-1")),
            "本来の exp を過ぎた退役世代は保持しない"
        );
    }

    #[test]
    fn code_attempts_burn_the_registration_at_the_limit() {
        let mut registration = PendingRegistration {
            jti: "j1".to_string(),
            sub: "s1".to_string(),
            user_id: vec![7; 16],
            access: Access::Rw,
            hmac_secret: vec![1; 32],
            code_hash: sha256_hex(b"123456"),
            code_attempts: 0,
            issued_label: None,
            expires_at_ms: REGISTRATION_TTL_MS,
        };
        assert_eq!(registration.check_code("123456"), CodeOutcome::Accepted);
        assert_eq!(registration.code_attempts, 0, "一致では加算しない");

        for remaining in (1..CODE_ATTEMPT_LIMIT).rev() {
            assert_eq!(
                registration.check_code("000000"),
                CodeOutcome::Rejected { remaining }
            );
        }
        assert_eq!(registration.check_code("000000"), CodeOutcome::Burned);
    }

    #[test]
    fn a_challenge_can_only_be_consumed_once() {
        // 決定 4: 同じ challenge が 2 回消費できない。
        let now = 1_000;
        let mut pending = PendingFile::default();
        pending.challenges.entry(endpoint()).or_default().insert(
            "c1".to_string(),
            PendingChallenge {
                id: "c1".to_string(),
                endpoint: endpoint(),
                purpose: ChallengePurpose::Assert,
                expires_at_ms: now + 60_000,
            },
        );
        assert!(
            pending
                .consume_challenge(&endpoint(), "c1", ChallengePurpose::Assert, now)
                .is_some()
        );
        assert!(
            pending
                .consume_challenge(&endpoint(), "c1", ChallengePurpose::Assert, now)
                .is_none(),
            "2 回目は消費できない"
        );
    }

    #[test]
    fn challenge_is_bound_to_its_endpoint_and_purpose() {
        let now = 1_000;
        let other = Endpoint::parse("https://hyoui-unstable.example.jp/").unwrap();
        let mut pending = PendingFile::default();
        pending.challenges.entry(endpoint()).or_default().insert(
            "c1".to_string(),
            PendingChallenge {
                id: "c1".to_string(),
                endpoint: endpoint(),
                purpose: ChallengePurpose::Assert,
                expires_at_ms: now + 60_000,
            },
        );
        // 別 endpoint では引けない (= すり替えが効かない、決定 3)。
        assert!(
            pending
                .consume_challenge(&other, "c1", ChallengePurpose::Assert, now)
                .is_none()
        );
        // 登録用として使い回せない。
        assert!(
            pending
                .consume_challenge(&endpoint(), "c1", ChallengePurpose::Register, now)
                .is_none()
        );
        // 期限切れも「無い」と同じ。
        assert!(
            pending
                .consume_challenge(&endpoint(), "c1", ChallengePurpose::Assert, now + 60_001)
                .is_none()
        );
    }

    #[test]
    fn sweep_drops_expired_entries_and_empty_buckets() {
        let mut pending = PendingFile::default();
        pending.challenges.entry(endpoint()).or_default().insert(
            "c1".to_string(),
            PendingChallenge {
                id: "c1".to_string(),
                endpoint: endpoint(),
                purpose: ChallengePurpose::Assert,
                expires_at_ms: 500,
            },
        );
        pending.sweep_expired(1_000);
        assert!(
            pending.challenges.is_empty(),
            "空になった endpoint の bucket も落とす"
        );
    }

    #[test]
    fn constant_time_eq_matches_plain_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn auth_file_round_trips_with_endpoint_keys() {
        // endpoint は key の 1 段 (決定 4)。JSON の object key になるので、
        // 正規形の文字列がそのまま key になる。
        let now = 1_000_000;
        let mut file = AuthFile::default();
        file.families
            .entry(endpoint())
            .or_default()
            .insert("fam1".to_string(), family(now));
        let json = serde_json::to_string(&file).unwrap();
        assert!(
            json.contains("\"https://hyoui.example.jp/\""),
            "key は正規形の endpoint: {json}"
        );
        let parsed: AuthFile = serde_json::from_str(&json).unwrap();
        assert!(parsed.families.contains_key(&endpoint()));
    }
}
