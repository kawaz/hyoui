//! access / refresh token、refresh cookie、登録 jwt (DR-0036 決定 2 / 決定 5)。
//!
//! ## token は署名しない
//!
//! access / refresh は opaque 乱数で、検証は record の lookup で行う (決定 5)。
//! 署名鍵を持つと保管・rotate・配布という管理対象が増えるが、record を引く形なら
//! 鍵なしで同じことが済む。
//!
//! ## 登録 jwt だけは署名する
//!
//! 招待 URL の fragment に載る jwt は **登録 1 本ごとの乱数 32 byte secret による
//! HMAC (HS256)** で、永続鍵を持たない (決定 2)。secret は CLI が `pending.json` に
//! 書き、gateway は読むだけである。**どの unit が POST を受けても検証できる**のは
//! secret がプロセスのメモリに無いからで、これが HA endpoint の登録を成立させている。

use serde::{Deserialize, Serialize};

pub use super::record::base64url;
use super::record::{Access, constant_time_eq, sha256_hex};
use crate::contract::Endpoint;

/// access / refresh / challenge id の乱数 byte 数。
const TOKEN_BYTES: usize = 32;

/// 乱数 token を base64url で 1 本作る。
///
/// **base64url なのは WS subprotocol に載せるためである** (決定 5) —
/// `Sec-WebSocket-Protocol` の値は RFC 6455 の token でなければならず、素の base64 の
/// `+` `/` `=` は使えない。
pub fn random_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; TOKEN_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    base64url(&bytes)
}

/// user handle (16 byte)。**sub ごとに 1 度だけ決めて使い回す** (決定 3)。
pub fn random_user_id() -> Vec<u8> {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes.to_vec()
}

/// 登録 jwt の HMAC secret (32 byte)。
pub fn random_secret() -> Vec<u8> {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.to_vec()
}

/// 6 桁コードの sha256 (hex)。`pending.json` に置くのはこの値だけで、コード自体は
/// CLI にしか出さない (決定 2)。
pub fn code_hash(code: &str) -> String {
    sha256_hex(code.as_bytes())
}

/// 6 桁コード。URL には含めず CLI にだけ表示する (決定 2)。
pub fn random_code() -> String {
    use rand::Rng;
    format!("{:06}", rand::rng().random_range(0..1_000_000u32))
}

// -----------------------------------------------------------------------------
// refresh cookie (決定 5)
// -----------------------------------------------------------------------------

/// refresh cookie の名前 — `__Secure-hyoui-<sha256(endpoint) 先頭 16 hex>`。
///
/// **`sub` を混ぜない。** `/auth/refresh` を受けた時点で server は**まだ誰の要求か
/// 知らない** (refresh token 自体が身元を答える値である)。名前を endpoint の
/// ハッシュだけにすると 1 つに決まり、prefix 一致の cookie を順に試す必要が消える。
///
/// `__Host-` ではなく `__Secure-` + `Path` を選ぶのは、同一 host の `/` と `/hyoui/`
/// を別 endpoint (別登録) として扱うため (`__Host-` は `Path=/` を強制する)。
pub fn cookie_name(endpoint: &Endpoint) -> String {
    let digest = sha256_hex(endpoint.as_str().as_bytes());
    format!("__Secure-hyoui-{}", &digest[..16])
}

/// refresh cookie を張る `Set-Cookie` の値。
///
/// `Path` は正規形 endpoint の path から末尾 `/` を落とした値 (決定 5)。
/// **`Path` は認可境界ではない** — 同一 origin の JS は任意の path に fetch でき、
/// `Path` は「ブラウザが自発的に付けて送る範囲」しか決めない。
pub fn set_cookie(endpoint: &Endpoint, refresh: &str, max_age_seconds: u64) -> String {
    format!(
        "{}={}; Max-Age={}; Path={}; HttpOnly; Secure; SameSite=Strict",
        cookie_name(endpoint),
        refresh,
        max_age_seconds,
        endpoint.cookie_path(),
    )
}

/// refresh cookie を落とす `Set-Cookie` の値 (= family 失効時)。
pub fn clear_cookie(endpoint: &Endpoint) -> String {
    format!(
        "{}=; Max-Age=0; Path={}; HttpOnly; Secure; SameSite=Strict",
        cookie_name(endpoint),
        endpoint.cookie_path(),
    )
}

/// `Cookie` ヘッダから当該 endpoint の refresh 値を取り出す。
pub fn refresh_from_cookie_header(header: &str, endpoint: &Endpoint) -> Option<String> {
    let name = cookie_name(endpoint);
    header.split(';').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key.trim() == name).then(|| value.trim().to_string())
    })
}

// -----------------------------------------------------------------------------
// 登録 jwt (決定 2)
// -----------------------------------------------------------------------------

/// 登録 jwt の claims (決定 2)。
///
/// reference の `iss` (発行 instance の id) は**持たない** — 発行するのは CLI で、
/// 検証するのはどの unit でもよいので、指す対象が無い。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistrationClaims {
    /// 登録先の `sub`。
    pub sub: String,
    /// 登録する endpoint (正規形)。
    pub endpoint: Endpoint,
    /// RP ID (= endpoint の hostname)。
    pub rp_id: String,
    /// user handle の base64url。
    pub user_id: String,
    /// claim (決定 7)。
    pub access: Access,
    /// 失効時刻 (unix 秒)。
    pub exp: u64,
    /// この登録 1 本の識別子。`pending.json` の key。
    pub jti: String,
}

/// jwt の扱いで起きる失敗。
///
/// **外に理由を出さない。** 攻撃者入力由来の失敗は一律「認証失敗」に翻訳する
/// (決定 5)。`Display` は log 用である。
#[derive(Debug, PartialEq, Eq)]
pub enum JwtError {
    /// 形が jwt でない (= 3 分割できない / base64url でない / JSON でない)。
    Malformed,
    /// alg が HS256 でない (= `none` 等へのすり替え)。
    UnexpectedAlgorithm,
    /// 署名が合わない。
    BadSignature,
    /// `exp` を過ぎている。
    Expired,
}

impl std::fmt::Display for JwtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            JwtError::Malformed => "registration token is malformed",
            JwtError::UnexpectedAlgorithm => "registration token uses an unexpected algorithm",
            JwtError::BadSignature => "registration token signature does not verify",
            JwtError::Expired => "registration token has expired",
        })
    }
}

impl std::error::Error for JwtError {}

/// HS256 の jwt を組む (= CLI が招待 URL の fragment に載せる値)。
pub fn encode_registration(claims: &RegistrationClaims, secret: &[u8]) -> String {
    let header = base64url(br#"{"alg":"HS256","typ":"JWT"}"#);
    let payload = base64url(
        serde_json::to_vec(claims)
            .expect("claims は必ず JSON になる")
            .as_slice(),
    );
    let signing_input = format!("{header}.{payload}");
    let signature = base64url(&hmac_sha256(secret, signing_input.as_bytes()));
    format!("{signing_input}.{signature}")
}

/// 署名を検証せずに `jti` だけ読む。
///
/// **secret を引くためだけに使う。** secret は `pending.json` の `jti` を key に
/// 置かれているので、検証の前に一度だけこの値が要る。ここで読んだ claims は
/// [`verify_registration`] を通るまで信じない。
pub fn peek_jti(jwt: &str) -> Result<String, JwtError> {
    Ok(decode_payload(jwt)?.jti)
}

/// jwt を検証して claims を返す。
///
/// `now_unix_ms` は呼び出し側の時刻。`exp` は秒なので ms から丸める。
pub fn verify_registration(
    jwt: &str,
    secret: &[u8],
    now_unix_ms: u64,
) -> Result<RegistrationClaims, JwtError> {
    let (signing_input, signature) = jwt.rsplit_once('.').ok_or(JwtError::Malformed)?;
    let header_segment = signing_input.split('.').next().ok_or(JwtError::Malformed)?;
    let header: JwtHeader = serde_json::from_slice(&decode_segment(header_segment)?)
        .map_err(|_| JwtError::Malformed)?;
    if header.alg != "HS256" {
        return Err(JwtError::UnexpectedAlgorithm);
    }
    let expected = base64url(&hmac_sha256(secret, signing_input.as_bytes()));
    if !constant_time_eq(expected.as_bytes(), signature.as_bytes()) {
        return Err(JwtError::BadSignature);
    }
    let claims = decode_payload(jwt)?;
    if claims.exp.saturating_mul(1000) <= now_unix_ms {
        return Err(JwtError::Expired);
    }
    Ok(claims)
}

#[derive(Deserialize)]
struct JwtHeader {
    alg: String,
}

fn decode_payload(jwt: &str) -> Result<RegistrationClaims, JwtError> {
    let mut segments = jwt.split('.');
    let _header = segments.next().ok_or(JwtError::Malformed)?;
    let payload = segments.next().ok_or(JwtError::Malformed)?;
    if segments.next().is_none() || segments.next().is_some() {
        return Err(JwtError::Malformed);
    }
    serde_json::from_slice(&decode_segment(payload)?).map_err(|_| JwtError::Malformed)
}

fn decode_segment(segment: &str) -> Result<Vec<u8>, JwtError> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(segment)
        .map_err(|_| JwtError::Malformed)
}

fn hmac_sha256(secret: &[u8], message: &[u8]) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    let mut mac =
        <Hmac<sha2::Sha256> as Mac>::new_from_slice(secret).expect("HMAC は任意長の鍵を受ける");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint() -> Endpoint {
        Endpoint::parse("https://hyoui.example.jp/").unwrap()
    }

    fn claims(exp: u64) -> RegistrationClaims {
        RegistrationClaims {
            sub: "hyoui.example.jp-1".to_string(),
            endpoint: endpoint(),
            rp_id: "hyoui.example.jp".to_string(),
            user_id: base64url(&[7u8; 16]),
            access: Access::Rw,
            exp,
            jti: "j1".to_string(),
        }
    }

    #[test]
    fn a_registration_token_round_trips_and_carries_its_jti_before_verification() {
        let secret = [3u8; 32];
        let jwt = encode_registration(&claims(2_000), &secret);
        // secret を引くために jti だけ先に読める (= まだ信じていない)。
        assert_eq!(peek_jti(&jwt).unwrap(), "j1");
        assert_eq!(
            verify_registration(&jwt, &secret, 1_000_000).unwrap(),
            claims(2_000)
        );
    }

    #[test]
    fn a_registration_token_signed_with_another_secret_is_rejected() {
        let jwt = encode_registration(&claims(2_000), &[3u8; 32]);
        assert_eq!(
            verify_registration(&jwt, &[4u8; 32], 1_000_000),
            Err(JwtError::BadSignature)
        );
    }

    #[test]
    fn the_alg_none_substitution_is_rejected() {
        // header を `{"alg":"none"}` に差し替えて署名を空にする古典。
        let secret = [3u8; 32];
        let jwt = encode_registration(&claims(2_000), &secret);
        let payload = jwt.split('.').nth(1).unwrap();
        let forged = format!("{}.{payload}.", base64url(br#"{"alg":"none","typ":"JWT"}"#));
        assert_eq!(
            verify_registration(&forged, &secret, 1_000_000),
            Err(JwtError::UnexpectedAlgorithm)
        );
    }

    #[test]
    fn an_expired_registration_token_is_rejected() {
        let secret = [3u8; 32];
        let jwt = encode_registration(&claims(1_000), &secret);
        assert_eq!(
            verify_registration(&jwt, &secret, 1_000_000),
            Err(JwtError::Expired),
            "exp は秒。1_000 秒 = 1_000_000 ms の時点で切れている"
        );
        assert!(verify_registration(&jwt, &secret, 999_999).is_ok());
    }

    #[test]
    fn malformed_tokens_do_not_panic() {
        for raw in ["", ".", "a.b", "a.b.c.d", "a.b.c", "$$.%%.^^"] {
            assert!(verify_registration(raw, &[0u8; 32], 0).is_err(), "{raw:?}");
        }
    }

    #[test]
    fn the_cookie_name_is_the_endpoint_hash_only() {
        // sub を混ぜない (決定 5) ので、同じ endpoint なら常に同じ 1 つの名前。
        let name = cookie_name(&endpoint());
        assert_eq!(name, cookie_name(&endpoint()));
        assert!(name.starts_with("__Secure-hyoui-"));
        assert_eq!(name.len(), "__Secure-hyoui-".len() + 16);
        // endpoint が違えば別 cookie になる (= endpoint ごとに別登録、決定 3)。
        let other = Endpoint::parse("https://hyoui-unstable.example.jp/").unwrap();
        assert_ne!(name, cookie_name(&other));
    }

    #[test]
    fn the_cookie_carries_the_decision_5_attributes() {
        let cookie = set_cookie(&endpoint(), "r1", 604_800);
        assert!(cookie.contains("HttpOnly"), "{cookie}");
        assert!(cookie.contains("Secure"), "{cookie}");
        assert!(cookie.contains("SameSite=Strict"), "{cookie}");
        assert!(cookie.contains("Path=/"), "{cookie}");

        // path 付き endpoint は末尾 `/` を落とした Path になる。
        let prefixed = Endpoint::parse("https://example.jp/hyoui/").unwrap();
        assert!(
            set_cookie(&prefixed, "r1", 1).contains("Path=/hyoui;"),
            "{}",
            set_cookie(&prefixed, "r1", 1)
        );
    }

    #[test]
    fn the_refresh_value_is_read_back_from_the_cookie_header() {
        let header = format!("other=1; {}=abc123; another=2", cookie_name(&endpoint()));
        assert_eq!(
            refresh_from_cookie_header(&header, &endpoint()),
            Some("abc123".to_string())
        );
        // 別 endpoint の cookie は引かない。
        let other = Endpoint::parse("https://hyoui-unstable.example.jp/").unwrap();
        assert_eq!(refresh_from_cookie_header(&header, &other), None);
    }

    #[test]
    fn random_values_have_the_expected_shape() {
        assert_ne!(random_token(), random_token());
        assert_eq!(random_user_id().len(), 16);
        assert_eq!(random_secret().len(), 32);
        let code = random_code();
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()), "{code}");
    }
}
