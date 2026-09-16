//! `/auth/*` と middleware の振る舞い (DR-0036 W2-3 の gate)。
//!
//! ここで固定するのは **署名を伴わない側** — 決定 1 の公開 / 保護の表、endpoint の
//! すり替え、6 桁コードの試行上限、refresh の rotate と再利用検知、失効の反映。
//!
//! **WebAuthn の署名経路は別の場所で見る。** 仮想 authenticator (softtoken) は
//! resident key を実装していないので (gate 4 の実測)、`residentKey: required` の
//! ままの登録 → 認証は実ブラウザ (Chrome の CDP 仮想 authenticator) でしか通せない。
//! crate の検証配線そのものは `auth::webauthn` の unit test が softtoken で見ており、
//! ここではその手前と後ろ (challenge の在庫管理、record の lookup、token の扱い) を
//! route 越しに固定する。

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use hyoui_web::auth::{
    AuthContext, AuthFile, PendingChallenge, PendingFile, PendingRegistration, REGISTRATION_TTL_MS,
    StateDir, token,
};
use hyoui_web::contract::{ChallengePurpose, Endpoint};
use tower::ServiceExt;

/// 隔離した `XDG_STATE_HOME` 上の router 1 つ (決定 9)。
struct Harness {
    app: axum::Router,
    context: AuthContext,
    /// **drop すると消えるので保持する。**
    _state: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let state = tempfile::tempdir().expect("tempdir");
        let context = AuthContext::at(StateDir::at(state.path()));
        Self {
            app: hyoui_web::router_with_auth(
                hyoui::config::Config::default(),
                None,
                context.clone(),
            ),
            context,
            _state: state,
        }
    }

    async fn send(&self, request: Request<Body>) -> axum::response::Response {
        self.app.clone().oneshot(request).await.unwrap()
    }

    async fn post_json(
        &self,
        uri: &str,
        body: serde_json::Value,
        cookie: Option<&str>,
    ) -> axum::response::Response {
        let request = Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json");
        let request = match cookie {
            Some(cookie) => request.header(header::COOKIE, cookie),
            None => request,
        };
        self.send(
            request
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
    }

    /// access token 1 本を持つ family を直に置く (= 登録 fixture、決定 9)。
    fn seed_family(&self, endpoint: &Endpoint, sub: &str) -> (String, String) {
        let access = token::random_token();
        let refresh = token::random_token();
        self.context
            .state_dir()
            .auth()
            .update::<AuthFile, _, _>(|file| {
                file.mint_family(
                    endpoint,
                    sub,
                    "fam-1".to_string(),
                    access.clone(),
                    refresh.clone(),
                    hyoui::time::now_unix_ms(),
                );
            })
            .expect("family を置く");
        (access, refresh)
    }
}

fn endpoint() -> Endpoint {
    Endpoint::parse("http://127.0.0.1:43690/").unwrap()
}

async fn error_code(response: axum::response::Response) -> String {
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body)
        .unwrap_or_else(|e| panic!("契約形のエラー body ({e}): {:?}", body));
    json["error"]["code"].as_str().unwrap().to_string()
}

/// 決定 1 の表そのもの。**何が公開で何が保護かを固定する。**
#[tokio::test]
async fn the_public_and_guarded_routes_follow_decision_1() {
    let harness = Harness::new();

    // 守らない: 可用性監視と `hyoui web daemon status` の入口、ログイン UI の材料、
    // session の内容を含まない静的 shell、認証そのものの経路。
    for uri in [
        "/healthz",
        "/version",
        "/",
        "/sessions/anything",
        "/assets/contract.js",
    ] {
        let response = harness
            .send(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await;
        assert_ne!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{uri} は認証境界を変えない (決定 1)"
        );
    }

    // 守る: session の画面内容と入力。
    for (method, uri) in [
        ("GET", "/api/sessions"),
        ("GET", "/api/sessions/x/screen"),
        ("POST", "/api/sessions/x/input"),
        ("POST", "/api/sessions/x/resize"),
        ("POST", "/api/sessions/x/resume"),
        ("GET", "/api/sessions/x/attach"),
    ] {
        let response = harness
            .send(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {uri} は 401 (決定 1)"
        );
        assert_eq!(error_code(response).await, "auth-required");
    }
}

/// access token は `Authorization: Bearer` でも WS subprotocol でも通る (決定 5)。
#[tokio::test]
async fn the_access_token_is_accepted_from_bearer_and_from_the_ws_subprotocol() {
    let harness = Harness::new();
    let (access, _) = harness.seed_family(&endpoint(), "sub-1");

    let response = harness
        .send(
            Request::builder()
                .uri("/api/sessions")
                .header(header::AUTHORIZATION, format!("Bearer {access}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    // WS upgrade でない要求でも、subprotocol に載った token で middleware は通る
    // (= 通った先の attach は upgrade の形が要るので 400 台になるが、401 ではない)。
    let response = harness
        .send(
            Request::builder()
                .uri("/api/sessions")
                .header("sec-websocket-protocol", format!("hyoui.token.{access}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    // 別の値では通らない。
    let response = harness
        .send(
            Request::builder()
                .uri("/api/sessions")
                .header(header::AUTHORIZATION, "Bearer not-the-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// 登録が 1 本も無い endpoint には認証 challenge を出さない。
#[tokio::test]
async fn an_assert_challenge_needs_a_registered_credential() {
    let harness = Harness::new();
    let response = harness
        .post_json(
            "/auth/challenge",
            serde_json::json!({"endpoint": endpoint().as_str()}),
            None,
        )
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(response).await, "auth-failed");
}

/// endpoint をすり替えた challenge は使えない (決定 3)。
#[tokio::test]
async fn a_challenge_cannot_be_used_against_another_endpoint() {
    let harness = Harness::new();
    let issued_for = endpoint();
    let other = Endpoint::parse("http://127.0.0.1:43691/").unwrap();
    let now_ms = hyoui::time::now_unix_ms();

    // 「A で取った challenge」を直に置き、B で使う。
    harness
        .context
        .state_dir()
        .pending()
        .update::<PendingFile, _, _>(|pending| {
            pending
                .challenges
                .entry(issued_for.clone())
                .or_default()
                .insert(
                    "c1".to_string(),
                    PendingChallenge {
                        id: "c1".to_string(),
                        endpoint: issued_for.clone(),
                        purpose: ChallengePurpose::Assert,
                        jti: None,
                        state: serde_json::Value::Null,
                        expires_at_ms: now_ms + 60_000,
                    },
                );
        })
        .expect("challenge を置く");

    let response = harness
        .post_json(
            "/auth/assert",
            serde_json::json!({
                "endpoint": other.as_str(),
                "challenge_id": "c1",
                "credential": {},
            }),
            None,
        )
        .await;
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "別 endpoint では引けない (決定 3)"
    );

    // 元の endpoint では引ける (= 引けた先で credential が無くて落ちるところまで)。
    // 失敗理由は分けないので、どちらも同じ code になる (決定 5)。
    let response = harness
        .post_json(
            "/auth/assert",
            serde_json::json!({
                "endpoint": issued_for.as_str(),
                "challenge_id": "c1",
                "credential": {},
            }),
            None,
        )
        .await;
    assert_eq!(error_code(response).await, "auth-failed");
}

/// 6 桁コードの誤入力は 5 回でその URL (jti) を焼く (決定 2)。
#[tokio::test]
async fn five_wrong_codes_burn_the_registration_url() {
    let harness = Harness::new();
    let endpoint = endpoint();
    let now_ms = hyoui::time::now_unix_ms();
    let secret = token::random_secret();
    let claims = token::RegistrationClaims {
        sub: "sub-1".to_string(),
        endpoint: endpoint.clone(),
        rp_id: endpoint.rp_id().to_string(),
        user_id: "AAAA".to_string(),
        access: hyoui_web::auth::Access::Rw,
        exp: now_ms / 1000 + 600,
        jti: "j1".to_string(),
    };
    let jwt = token::encode_registration(&claims, &secret);
    harness
        .context
        .state_dir()
        .pending()
        .update::<PendingFile, _, _>(|pending| {
            pending
                .registrations
                .entry(endpoint.clone())
                .or_default()
                .insert(
                    "j1".to_string(),
                    PendingRegistration {
                        jti: "j1".to_string(),
                        sub: "sub-1".to_string(),
                        user_id: vec![7; 16],
                        access: hyoui_web::auth::Access::Rw,
                        hmac_secret: secret.clone(),
                        code_hash: token::code_hash("123456"),
                        code_attempts: 0,
                        issued_label: None,
                        expires_at_ms: now_ms + REGISTRATION_TTL_MS,
                    },
                );
        })
        .expect("登録を置く");

    let body = |code: &str| {
        serde_json::json!({
            "endpoint": endpoint.as_str(),
            "challenge_id": "c-missing",
            "jwt": jwt,
            "code": code,
            "credential": {},
        })
    };

    for attempt in 1..=5 {
        let response = harness
            .post_json("/auth/register", body("000000"), None)
            .await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{attempt} 回目の誤入力"
        );
    }

    // 焼けた後は正しいコードでも通らない (= 登録 URL の再発行が要る)。
    let pending: PendingFile = harness
        .context
        .state_dir()
        .pending()
        .read()
        .expect("pending を読む");
    assert!(
        pending
            .registrations
            .get(&endpoint)
            .is_none_or(|by_jti| !by_jti.contains_key("j1")),
        "5 回で jti が焼ける (決定 2)"
    );
    let response = harness
        .post_json("/auth/register", body("123456"), None)
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// 正しいコードでも、**WebAuthn の検証に届く前に落ちた分では jti を焼かない** (決定 2)。
#[tokio::test]
async fn a_failed_webauthn_step_does_not_burn_the_registration_url() {
    let harness = Harness::new();
    let endpoint = endpoint();
    let now_ms = hyoui::time::now_unix_ms();
    let secret = token::random_secret();
    let jwt = token::encode_registration(
        &token::RegistrationClaims {
            sub: "sub-1".to_string(),
            endpoint: endpoint.clone(),
            rp_id: endpoint.rp_id().to_string(),
            user_id: "AAAA".to_string(),
            access: hyoui_web::auth::Access::Rw,
            exp: now_ms / 1000 + 600,
            jti: "j1".to_string(),
        },
        &secret,
    );
    harness
        .context
        .state_dir()
        .pending()
        .update::<PendingFile, _, _>(|pending| {
            pending
                .registrations
                .entry(endpoint.clone())
                .or_default()
                .insert(
                    "j1".to_string(),
                    PendingRegistration {
                        jti: "j1".to_string(),
                        sub: "sub-1".to_string(),
                        user_id: vec![7; 16],
                        access: hyoui_web::auth::Access::Rw,
                        hmac_secret: secret.clone(),
                        code_hash: token::code_hash("123456"),
                        code_attempts: 0,
                        issued_label: None,
                        expires_at_ms: now_ms + REGISTRATION_TTL_MS,
                    },
                );
        })
        .expect("登録を置く");

    // challenge が無いので落ちる。**コードは合っている。**
    let response = harness
        .post_json(
            "/auth/register",
            serde_json::json!({
                "endpoint": endpoint.as_str(),
                "challenge_id": "c-missing",
                "jwt": jwt,
                "code": "123456",
                "credential": {},
            }),
            None,
        )
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let pending: PendingFile = harness
        .context
        .state_dir()
        .pending()
        .read()
        .expect("pending を読む");
    let registration = pending
        .registrations
        .get(&endpoint)
        .and_then(|by_jti| by_jti.get("j1"))
        .expect("jti は焼けていない (決定 2 の検証順序)");
    assert_eq!(
        registration.code_attempts, 0,
        "正しいコードでは試行回数を増やさない"
    );
}

/// refresh は使うたび rotate し、再提示は family ごと失効させる (決定 5)。
#[tokio::test]
async fn refresh_rotates_and_a_reused_value_revokes_the_family() {
    let harness = Harness::new();
    let endpoint = endpoint();
    let (access, refresh) = harness.seed_family(&endpoint, "sub-1");
    let cookie = |value: &str| format!("{}={value}", token::cookie_name(&endpoint));
    let body = serde_json::json!({"endpoint": endpoint.as_str()});

    let response = harness
        .post_json("/auth/refresh", body.clone(), Some(&cookie(&refresh)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let set_cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .expect("refresh は cookie で返す (決定 5)")
        .to_string();
    assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");
    assert!(set_cookie.contains("SameSite=Strict"), "{set_cookie}");
    let rotated = set_cookie
        .split(';')
        .next()
        .and_then(|pair| pair.split_once('='))
        .map(|(_, value)| value.to_string())
        .expect("cookie の値");
    assert_ne!(rotated, refresh, "使うたび rotate する");

    let payload = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(
        json["access_token"].as_str(),
        Some(access.as_str()),
        "access は残り寿命が半分以上あれば据え置く (決定 5)"
    );
    assert!(
        json["expires_at"]
            .as_str()
            .is_some_and(|v| v.ends_with('Z'))
    );
    assert_eq!(json["sub"], "sub-1");
    assert!(
        payload
            .windows(rotated.len())
            .all(|w| w != rotated.as_bytes()),
        "refresh を body に載せない (決定 5)"
    );

    // 直前 1 世代は 60 秒の猶予で同じ答えを返す (rotate しない)。
    let response = harness
        .post_json("/auth/refresh", body.clone(), Some(&cookie(&refresh)))
        .await;
    assert_eq!(response.status(), StatusCode::OK, "猶予内の再送は通る");

    // 猶予を潰す (= 退役世代の retired_at を過去にずらす) と、再提示で family が畳まれる。
    harness
        .context
        .state_dir()
        .auth()
        .update::<AuthFile, _, _>(|file| {
            for family in file
                .families
                .get_mut(&endpoint)
                .expect("family")
                .values_mut()
            {
                for retired in &mut family.retired {
                    retired.retired_at_ms = 0;
                }
            }
        })
        .expect("猶予を潰す");
    let response = harness
        .post_json("/auth/refresh", body.clone(), Some(&cookie(&refresh)))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(
        response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("Max-Age=0")),
        "失効時は cookie も落とす"
    );

    // family ごと失効したので、同じ family の access も通らなくなる。
    let response = harness
        .send(
            Request::builder()
                .uri("/api/sessions")
                .header(header::AUTHORIZATION, format!("Bearer {access}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "再利用検知は family ごと失効させる (決定 5)"
    );
}

/// cookie が無い / 別 endpoint の cookie しか無い refresh は通らない。
#[tokio::test]
async fn refresh_needs_the_cookie_for_that_endpoint() {
    let harness = Harness::new();
    let endpoint = endpoint();
    let (_, refresh) = harness.seed_family(&endpoint, "sub-1");
    let other = Endpoint::parse("http://127.0.0.1:43691/").unwrap();
    let body = serde_json::json!({"endpoint": endpoint.as_str()});

    let response = harness.post_json("/auth/refresh", body.clone(), None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // 値は正しいが、別 endpoint の名前で入っている cookie は引かれない (決定 5)。
    let response = harness
        .post_json(
            "/auth/refresh",
            body,
            Some(&format!("{}={refresh}", token::cookie_name(&other))),
        )
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// `passkey remove` 相当の tombstone が次の要求で効く (決定 4)。
#[tokio::test]
async fn a_tombstoned_family_stops_working_on_the_next_request() {
    let harness = Harness::new();
    let endpoint = endpoint();
    let (access, refresh) = harness.seed_family(&endpoint, "sub-1");

    harness
        .context
        .state_dir()
        .auth()
        .update::<AuthFile, _, _>(|file| {
            file.tombstone_sub("sub-1", hyoui::time::now_unix_ms());
        })
        .expect("tombstone");

    let response = harness
        .send(
            Request::builder()
                .uri("/api/sessions")
                .header(header::AUTHORIZATION, format!("Bearer {access}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "新規要求が落ちる"
    );

    let response = harness
        .post_json(
            "/auth/refresh",
            serde_json::json!({"endpoint": endpoint.as_str()}),
            Some(&format!("{}={refresh}", token::cookie_name(&endpoint))),
        )
        .await;
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "`/auth/refresh` も落ちる"
    );
}

/// `/auth/*` の body は 64 KiB で切る (決定 5)。
#[tokio::test]
async fn an_oversized_auth_body_is_refused() {
    let harness = Harness::new();
    let oversized = "x".repeat(64 * 1024 + 1);
    let response = harness
        .post_json(
            "/auth/challenge",
            serde_json::json!({"endpoint": endpoint().as_str(), "jwt": oversized}),
            None,
        )
        .await;
    assert_ne!(
        response.status(),
        StatusCode::OK,
        "64 KiB を超える body は受けない"
    );
    assert!(
        response.status() == StatusCode::PAYLOAD_TOO_LARGE
            || response.status() == StatusCode::BAD_REQUEST,
        "status={}",
        response.status()
    );
}
