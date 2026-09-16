//! WebAuthn の登録 / 認証 (DR-0036 決定 2 / 決定 3 / 決定 8)。
//!
//! 検証手順 (WebAuthn L2 §7.1 / §7.2) と COSE / CBOR の解釈は `webauthn-rs-core` に
//! 委ね、ここが書くのは **RP を record の endpoint から組むこと** と、crate が見ない
//! 2 点 (`crossOrigin` の登録時拒否、`userHandle` の照合) である。
//!
//! ## RP は record の endpoint から決まる
//!
//! gateway は自分の endpoint を知らない (決定 3)。`Webauthn` 相当の検証器は要求ごとに
//! endpoint から組み、`rp_id` は endpoint の hostname、許す origin は endpoint の
//! origin 1 つだけにする。**registrable suffix は許さない** — suffix を名乗れると、その
//! 配下の全ホストでその credential が使えることになる。
//!
//! ## 高レベル API を使わない理由
//!
//! `webauthn-rs` の `start_passkey_registration` は `residentKey: "discouraged"` を
//! hardcode する。決定 2 は `required` なので、`WebauthnCore` の builder を使う。
//! 構築子の名が `new_unsafe_experts_only` なのは「既定から外れる設定を自分で選ぶ」
//! 意味で、ここではまさに resident key と origin の扱いを自分で決めている。

use std::time::Duration;

use webauthn_rs_core::WebauthnCore;
use webauthn_rs_core::proto::{
    AttestationConveyancePreference, AuthenticationState, COSEAlgorithm, Credential, CredentialID,
    PublicKeyCredential, RegisterPublicKeyCredential, RegistrationState, UserVerificationPolicy,
};

use crate::contract::Endpoint;

/// challenge の寿命。登録 URL の寿命 (10 分) より短くする理由が無いので揃える。
const CHALLENGE_TIMEOUT: Duration = Duration::from_secs(600);

/// WebAuthn の検証で起きる失敗。
///
/// **理由を細かく外に出さない。** 攻撃者入力由来の例外は一律「認証失敗」に翻訳し、
/// 500 にしない (決定 5 が ccmsg から借りる実装判断)。`Display` は log 用である。
#[derive(Debug)]
pub enum WebauthnFailure {
    /// RP の組み立てに失敗した (= endpoint が RP ID として使えない)。
    UnusableEndpoint(String),
    /// crate の検証が落ちた。
    Rejected(String),
    /// iframe から登録しようとした (決定 6 — 登録は top-level だけ)。
    CrossOriginRegistration,
    /// assertion の `userHandle` が record の `user_id` と一致しない (決定 2)。
    UserHandleMismatch,
}

impl std::fmt::Display for WebauthnFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WebauthnFailure::UnusableEndpoint(reason) => {
                write!(f, "endpoint cannot be used as a WebAuthn RP: {reason}")
            }
            WebauthnFailure::Rejected(reason) => {
                write!(f, "webauthn verification failed: {reason}")
            }
            WebauthnFailure::CrossOriginRegistration => {
                f.write_str("registration must run in a top-level context")
            }
            WebauthnFailure::UserHandleMismatch => {
                f.write_str("assertion user handle does not match the record")
            }
        }
    }
}

impl std::error::Error for WebauthnFailure {}

type Result<T> = std::result::Result<T, WebauthnFailure>;

/// 1 つの endpoint に対する RP。要求ごとに組む (= gateway は endpoint を持たない)。
pub struct Rp {
    core: WebauthnCore,
    /// 決定 2 の `residentKey: "required"`。production では常に `true`。
    require_resident_key: bool,
}

impl Rp {
    /// record / 要求の endpoint から RP を組む。
    ///
    /// 許す origin は endpoint の origin 1 つだけ。subdomain も任意 port も許さない
    /// (= `allow_subdomains_origin` / `allow_any_port` を立てない)。
    pub fn for_endpoint(endpoint: &Endpoint) -> Result<Self> {
        Self::build(endpoint, true)
    }

    /// `residentKey` の要求だけ下げた RP。**test 専用。**
    ///
    /// `webauthn-authenticator-rs` の仮想 authenticator (`SoftToken` /
    /// `SoftPasskey`) はどちらも resident key を実装していない
    /// (`if resident_key { return Err(NotSupported) }`) ため、crate の検証経路が
    /// 繋がっていることを Rust の test で通すにはこの 1 設定を下げる必要がある。
    /// **`residentKey: required` のままの経路は実ブラウザで確認する** (gate 3 で
    /// Chrome の仮想 authenticator が `residentKey: "required"` の登録を通すことを
    /// 実測済み、通しの登録 → 認証は W2-3 の実機確認で見る)。
    #[cfg(test)]
    fn for_endpoint_without_resident_key(endpoint: &Endpoint) -> Result<Self> {
        Self::build(endpoint, false)
    }

    fn build(endpoint: &Endpoint, require_resident_key: bool) -> Result<Self> {
        let origin = url::Url::parse(endpoint.origin())
            .map_err(|e| WebauthnFailure::UnusableEndpoint(e.to_string()))?;
        let core = WebauthnCore::new_unsafe_experts_only(
            "hyoui",
            endpoint.rp_id(),
            vec![origin],
            CHALLENGE_TIMEOUT,
            // registrable suffix を許さない (決定 3)。
            Some(false),
            // port 違いを同一視しない (= endpoint ごとに別登録、決定 3)。
            Some(false),
        );
        Ok(Self {
            core,
            require_resident_key,
        })
    }

    /// 登録の challenge を作る (決定 2 の `create()` 設定)。
    ///
    /// `attestation: none` — 「この credential を作ってよい人か」は jwt と 6 桁コードが
    /// 既に担保しており、authenticator の出自証明は要件に無い。
    pub fn start_registration(
        &self,
        user_id: &[u8],
        sub: &str,
    ) -> Result<(
        webauthn_rs_core::proto::CreationChallengeResponse,
        RegistrationState,
    )> {
        let builder = self
            .core
            .new_challenge_register_builder(user_id, sub, sub)
            .map_err(|e| WebauthnFailure::Rejected(e.to_string()))?
            .attestation(AttestationConveyancePreference::None)
            .credential_algorithms(vec![
                COSEAlgorithm::ES256,
                COSEAlgorithm::RS256,
                COSEAlgorithm::EDDSA,
            ])
            // 決定 2: discoverable credential にする。高レベル API は
            // `discouraged` を hardcode するのでここを builder で指定する。
            .require_resident_key(self.require_resident_key)
            .user_verification_policy(UserVerificationPolicy::Required)
            .authenticator_attachment(None);
        self.core
            .generate_challenge_register(builder)
            .map_err(|e| WebauthnFailure::Rejected(e.to_string()))
    }

    /// 登録を検証して credential を得る。
    ///
    /// **`crossOrigin: true` は crate が登録経路で拒否する** ので、hyoui 側で足す
    /// 判定は無い (決定 6 / gate 4 の実測)。念のため同じ判定を自分でも行い、crate の
    /// 挙動が変わった時に黙って iframe 登録が通らないよう固定する。
    pub fn finish_registration(
        &self,
        response: &RegisterPublicKeyCredential,
        state: &RegistrationState,
    ) -> Result<Credential> {
        if client_data_says_cross_origin(&response.response.client_data_json) {
            return Err(WebauthnFailure::CrossOriginRegistration);
        }
        self.core
            .register_credential(response, state, None)
            .map_err(|e| WebauthnFailure::Rejected(format!("{e:?}")))
    }

    /// 認証の challenge を作る。
    ///
    /// 渡す credential は **その endpoint の record だけ**に絞ってある (決定 3 — 要求の
    /// `endpoint` で record 集合を絞る)。
    pub fn start_authentication(
        &self,
        credentials: Vec<Credential>,
    ) -> Result<(
        webauthn_rs_core::proto::RequestChallengeResponse,
        AuthenticationState,
    )> {
        let builder = self
            .core
            .new_challenge_authenticate_builder(credentials, Some(UserVerificationPolicy::Required))
            .map_err(|e| WebauthnFailure::Rejected(e.to_string()))?;
        self.core
            .generate_challenge_authenticate(builder)
            .map_err(|e| WebauthnFailure::Rejected(e.to_string()))
    }

    /// assertion を検証する。
    ///
    /// **`crossOrigin` は見ない** (決定 6) — cross-origin iframe の `get()` では Chrome が
    /// `true` を送るので、拒否すると ccmsg の Terminal タブが必ず落ちる。この経路で
    /// 「この endpoint のページで get が走った」を担保するのは `rpIdHash` と
    /// `clientDataJSON.origin` の一致で、どちらも crate が検証する。
    ///
    /// **`userHandle` は crate が検証しない**ので (gate 4 の実測)、ここで照合する。
    /// `residentKey: required` なので handle は常に載る (決定 2)。
    pub fn finish_authentication(
        &self,
        response: &PublicKeyCredential,
        state: &AuthenticationState,
        expected_user_id: &[u8],
    ) -> Result<webauthn_rs_core::proto::AuthenticationResult> {
        let presented = response
            .response
            .user_handle
            .as_ref()
            .map(|handle| handle.as_ref())
            .ok_or(WebauthnFailure::UserHandleMismatch)?;
        if !super::record::constant_time_eq(presented, expected_user_id) {
            return Err(WebauthnFailure::UserHandleMismatch);
        }
        self.core
            .authenticate_credential(response, state)
            .map_err(|e| WebauthnFailure::Rejected(format!("{e:?}")))
    }
}

/// credential id を **バイト比較** で引く (決定 5)。
///
/// base64url の表現が正規形でない (padding / alphabet の揺れ) ので、文字列で引くと
/// 同じ credential を別物として扱いうる。
pub fn find_by_credential_id<'a, T>(
    records: impl IntoIterator<Item = (&'a CredentialID, T)>,
    presented: &CredentialID,
) -> Option<T> {
    records.into_iter().find_map(|(id, value)| {
        super::record::constant_time_eq(id.as_ref(), presented.as_ref()).then_some(value)
    })
}

/// `clientDataJSON` が `crossOrigin: true` を名乗っているか。
///
/// **present であることは要求しない** — Chrome 系は最上位フレームでも常に `false` を
/// 送るが、送らない実装もある (reference)。`true` のときだけ拒む。
fn client_data_says_cross_origin(client_data_json: &[u8]) -> bool {
    #[derive(serde::Deserialize)]
    struct ClientData {
        #[serde(rename = "crossOrigin", default)]
        cross_origin: bool,
    }
    serde_json::from_slice::<ClientData>(client_data_json)
        .map(|data| data.cross_origin)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint() -> Endpoint {
        Endpoint::parse("https://hyoui.example.jp/").unwrap()
    }

    #[test]
    fn registration_challenge_carries_the_decision_2_settings() {
        // gate 4: attestation none / UV required / residentKey required。
        let rp = Rp::for_endpoint(&endpoint()).unwrap();
        let (challenge, _state) = rp
            .start_registration(&[7u8; 16], "hyoui.example.jp-1")
            .unwrap();
        let json = serde_json::to_value(&challenge).unwrap();
        let options = &json["publicKey"];
        assert_eq!(options["attestation"], "none");
        assert_eq!(
            options["authenticatorSelection"]["userVerification"],
            "required"
        );
        assert_eq!(
            options["authenticatorSelection"]["residentKey"], "required",
            "決定 2: discoverable credential にする"
        );
        assert_eq!(options["rp"]["id"], "hyoui.example.jp");
    }

    #[test]
    fn rp_is_built_from_the_record_endpoint() {
        // gateway は自分の endpoint を知らない (決定 3)。RP は endpoint から組む。
        let prefixed = Endpoint::parse("https://example.jp/hyoui/").unwrap();
        let rp = Rp::for_endpoint(&prefixed).unwrap();
        let (challenge, _) = rp.start_registration(&[1u8; 16], "s").unwrap();
        let json = serde_json::to_value(&challenge).unwrap();
        // RP ID は hostname。path は RP ID に出ない (= 同一 origin の path 分離は
        // 認可境界にならない、決定 3)。
        assert_eq!(json["publicKey"]["rp"]["id"], "example.jp");
    }

    #[test]
    fn cross_origin_client_data_is_detected_only_when_true() {
        assert!(client_data_says_cross_origin(br#"{"crossOrigin":true}"#));
        assert!(!client_data_says_cross_origin(br#"{"crossOrigin":false}"#));
        // present でないことは拒否の理由にしない (reference)。
        assert!(!client_data_says_cross_origin(
            br#"{"type":"webauthn.create"}"#
        ));
        assert!(!client_data_says_cross_origin(b"not json"));
    }

    #[test]
    fn credential_id_lookup_compares_bytes() {
        // base64url が正規形でないのでバイト比較で引く (決定 5)。
        let a = CredentialID::from(vec![1, 2, 3]);
        let b = CredentialID::from(vec![1, 2, 4]);
        let records = vec![(&a, "first"), (&b, "second")];
        assert_eq!(
            find_by_credential_id(records.clone(), &CredentialID::from(vec![1, 2, 4])),
            Some("second")
        );
        assert_eq!(
            find_by_credential_id(records, &CredentialID::from(vec![9])),
            None
        );
    }

    /// 検証経路が端から端まで繋がっている (gate 4)。
    ///
    /// **`residentKey` だけ下げている。** 仮想 authenticator が resident key 非対応
    /// なので、この test が覆うのは challenge 生成 → 署名 → `register_credential` →
    /// 認証 challenge → `authenticate_credential` → `userHandle` 照合の配線であって、
    /// `residentKey: required` そのものではない (そちらは実ブラウザで確認する)。
    #[test]
    fn verification_pipeline_is_wired_end_to_end() {
        use webauthn_authenticator_rs::AuthenticatorBackend;
        use webauthn_authenticator_rs::softtoken::SoftToken;

        let endpoint = endpoint();
        let rp = Rp::for_endpoint_without_resident_key(&endpoint).unwrap();
        let origin = url::Url::parse(endpoint.origin()).unwrap();
        let user_id = [7u8; 16];

        let (mut client, _ca) = SoftToken::new(true).expect("softtoken");

        let (challenge, reg_state) = rp.start_registration(&user_id, "sub-1").unwrap();
        let response = client
            .perform_register(origin.clone(), challenge.public_key, 60_000)
            .expect("softtoken registration");
        let credential = rp
            .finish_registration(&response, &reg_state)
            .expect("finish_registration");

        let (challenge, auth_state) = rp.start_authentication(vec![credential.clone()]).unwrap();
        let mut assertion = client
            .perform_auth(origin, challenge.public_key, 60_000)
            .expect("softtoken authentication");

        // **`residentKey` を下げると assertion に handle が載らない。** これが決定 2 で
        // `required` を要る理由そのものなので、test で固定する。
        assert!(
            assertion.response.user_handle.is_none(),
            "非 resident key の credential は handle を返さない"
        );
        let err = rp
            .finish_authentication(&assertion, &auth_state, &user_id)
            .expect_err("handle が無ければ通さない");
        assert!(matches!(err, WebauthnFailure::UserHandleMismatch), "{err}");

        // handle は署名対象ではないので、`required` の実機で載ってくる状態を
        // 注入して再現できる。これで照合と crate の署名検証の両方を通す。
        assertion.response.user_handle = Some(user_id.to_vec());
        let result = rp
            .finish_authentication(&assertion, &auth_state, &user_id)
            .expect("handle が一致すれば通る");
        assert!(result.user_verified(), "UV required を通っている");

        // 別の handle を期待すると落ちる (= 照合が効いている)。
        let (challenge, auth_state) = rp.start_authentication(vec![credential]).unwrap();
        let mut assertion = client
            .perform_auth(
                url::Url::parse(endpoint.origin()).unwrap(),
                challenge.public_key,
                60_000,
            )
            .expect("softtoken authentication");
        assertion.response.user_handle = Some(user_id.to_vec());
        let err = rp
            .finish_authentication(&assertion, &auth_state, &[9u8; 16])
            .expect_err("handle 不一致は落ちる");
        assert!(matches!(err, WebauthnFailure::UserHandleMismatch), "{err}");
    }
}
