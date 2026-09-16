//! `/auth/*` の 4 経路と `/api/*` を守る middleware (DR-0036 決定 1 / 3 / 5)。
//!
//! ## 守る範囲
//!
//! 守るのは `/api/*` と WS attach への到達 (= session の画面内容と入力)。
//! `/healthz` `/version` `/assets/*` `/` `/sessions/{id}` `/auth/*` は通す (決定 1)。
//! HTML を無認証で配る帰結として、ログイン状態の判定は JS が `/api/*` の 401 を
//! 受けて行い、ページ内に overlay でログイン UI を出す。
//!
//! ## 失敗の出しかた
//!
//! 攻撃者入力由来の失敗は一律 `auth-failed` の 401 に翻訳し、500 にしない。
//! **URL が違うのかコードが違うのか challenge が無いのかを応答で分けない** (決定 5)。
//! log には理由を出す (= 運用者は手元の log で切り分けられる)。
//!
//! ## 検証の順序 (決定 2)
//!
//! `/auth/register` は「6 桁コード → WebAuthn 登録検証 → 公開鍵の import 可否 →
//! **通ってから** jti と challenge を消費」の順で進む。逆順だと一時的な失敗 1 回で
//! 登録 URL が焼ける。6 桁コードだけは外した時点で試行回数を加算する (= それが
//! 総当たりの上限そのもの) ので、lock 下の read-modify-write で数える。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use webauthn_rs_core::proto::{
    AuthenticationState, PublicKeyCredential, RegisterPublicKeyCredential, RegistrationState,
};

use super::record::{
    AuthFile, CodeOutcome, CredentialRecord, FamilyRecord, PendingChallenge, PendingFile,
    REFRESH_TTL_MS, RefreshOutcome,
};
use super::store::StateDir;
use super::token;
use super::webauthn::Rp;
use crate::AppState;
use crate::contract::{
    AssertRequest, ChallengePurpose, ChallengeRequest, ChallengeResponse, Endpoint, ErrorInfo,
    RefreshRequest, RegisterRequest, SessionResponse, code,
};

/// `/auth/*` 共通の body 上限 (64 KiB、決定 5)。
const AUTH_BODY_LIMIT: usize = 64 * 1024;

/// `/auth/*` 共通の rate limit (30 req/s、決定 5)。
const AUTH_RATE_PER_SECOND: u32 = 30;

/// WS の access token を運ぶ subprotocol の接頭辞 (決定 5)。
pub const WS_TOKEN_PROTOCOL_PREFIX: &str = "hyoui.token.";

/// challenge の寿命 (10 分)。登録 URL と揃える。
const CHALLENGE_TTL_MS: u64 = 10 * 60 * 1000;

// -----------------------------------------------------------------------------
// state
// -----------------------------------------------------------------------------

/// 認証が使う state (= 登録簿の置き場と rate limit の計数)。
///
/// **endpoint は持たない。** gateway は自分の endpoint を知らない (決定 3)。
#[derive(Clone)]
pub struct AuthContext {
    state_dir: Arc<StateDir>,
    limiter: Arc<Mutex<RateLimiter>>,
}

impl AuthContext {
    /// 既定の置き場 (`$XDG_STATE_HOME/hyoui-web/`) で組む。
    pub fn new() -> Self {
        Self::at(StateDir::default_root())
    }

    /// 置き場を明示して組む (= test の隔離 `XDG_STATE_HOME`、決定 9)。
    pub fn at(state_dir: StateDir) -> Self {
        Self {
            state_dir: Arc::new(state_dir),
            limiter: Arc::new(Mutex::new(RateLimiter::new(AUTH_RATE_PER_SECOND))),
        }
    }

    /// 登録簿の置き場。
    pub fn state_dir(&self) -> &StateDir {
        &self.state_dir
    }

    /// 置き場の共有 handle (= WS へ持ち込む分)。
    pub(crate) fn state_dir_handle(&self) -> Arc<StateDir> {
        self.state_dir.clone()
    }
}

impl Default for AuthContext {
    fn default() -> Self {
        Self::new()
    }
}

/// 1 秒の固定窓で数える rate limit。
///
/// Design rationale: 窓を跨ぐ瞬間に上限の 2 倍が通りうる代わりに、状態が
/// (窓の開始, 件数) の 2 つだけで済む。守りたいのは 6 桁コードと credential の
/// 総当たりで、そちらは試行回数の上限 (決定 2) が独立に効く。
struct RateLimiter {
    limit: u32,
    window_started: Instant,
    seen: u32,
}

impl RateLimiter {
    fn new(limit: u32) -> Self {
        Self {
            limit,
            window_started: Instant::now(),
            seen: 0,
        }
    }

    fn allow(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.window_started) >= Duration::from_secs(1) {
            self.window_started = now;
            self.seen = 0;
        }
        self.seen += 1;
        self.seen <= self.limit
    }
}

/// middleware が通した要求の身元。handler は request extension から読む。
#[derive(Debug, Clone)]
pub struct Identity {
    /// 誰として通ったか (= credential の `sub`)。
    pub sub: String,
    /// どの endpoint の family か。
    pub endpoint: Endpoint,
    /// access の期限 (unix ms)。`hello.auth_expires_at` に載る。
    pub access_expires_at_ms: u64,
}

// -----------------------------------------------------------------------------
// router
// -----------------------------------------------------------------------------

/// `/auth/*` の 4 経路。body 上限と rate limit を共通で被せる (決定 5)。
pub(crate) fn routes(auth: AuthContext) -> Router<AppState> {
    Router::new()
        .route("/auth/challenge", post(post_challenge))
        .route("/auth/register", post(post_register))
        .route("/auth/assert", post(post_assert))
        .route("/auth/refresh", post(post_refresh))
        .layer(DefaultBodyLimit::max(AUTH_BODY_LIMIT))
        .route_layer(axum::middleware::from_fn_with_state(auth, rate_limit))
}

async fn rate_limit(State(auth): State<AuthContext>, request: Request, next: Next) -> Response {
    let allowed = auth
        .limiter
        .lock()
        .map(|mut limiter| limiter.allow())
        .unwrap_or(true);
    if !allowed {
        return failure(
            StatusCode::TOO_MANY_REQUESTS,
            code::RATE_LIMITED,
            "too many authentication requests",
        );
    }
    next.run(request).await
}

/// `/api/*` と WS attach を守る (決定 1)。
///
/// token は `Authorization: Bearer` か、WS の subprotocol
/// `hyoui.token.<base64url>` で受ける (決定 5)。**family の検証は cache を使わず
/// 毎回 file を読む** — rotate の直後に他 unit が古い cache で判定すると
/// 再利用検知が誤発火する (決定 4)。
pub(crate) async fn require_auth(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(presented) = presented_token(request.headers()) else {
        return failure(
            StatusCode::UNAUTHORIZED,
            code::AUTH_REQUIRED,
            "this endpoint requires an authenticated session",
        );
    };
    let now_ms = hyoui::time::now_unix_ms();
    let file: AuthFile = match state.auth.state_dir.auth().read() {
        Ok(file) => file,
        Err(e) => {
            eprintln!("hyoui-web: auth.json read failed: {e}");
            return failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                code::INTERNAL_ERROR,
                "authentication state is unreadable",
            );
        }
    };
    let Some(family) = file.find_live_family_by_access(&presented, now_ms) else {
        return failure(
            StatusCode::UNAUTHORIZED,
            code::AUTH_REQUIRED,
            "this endpoint requires an authenticated session",
        );
    };
    request.extensions_mut().insert(Identity {
        sub: family.sub.clone(),
        endpoint: family.endpoint.clone(),
        access_expires_at_ms: family.access.expires_at_ms,
    });
    next.run(request).await
}

/// `Authorization: Bearer` と WS subprotocol の両方から access token を拾う。
fn presented_token(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    {
        let value = value.trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    ws_token_protocol(headers).map(|(_, token)| token)
}

/// `Sec-WebSocket-Protocol` から `hyoui.token.<値>` を 1 つ拾う。
///
/// 返すのは (subprotocol 全体, token)。**選んだ subprotocol はそのまま echo する**
/// 必要があるので (決定 5)、値の側だけでなく元の文字列も返す。
pub(crate) fn ws_token_protocol(headers: &HeaderMap) -> Option<(String, String)> {
    headers
        .get_all("sec-websocket-protocol")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .find_map(|protocol| {
            let token = protocol.strip_prefix(WS_TOKEN_PROTOCOL_PREFIX)?;
            (!token.is_empty()).then(|| (protocol.to_string(), token.to_string()))
        })
}

// -----------------------------------------------------------------------------
// POST /auth/challenge
// -----------------------------------------------------------------------------

async fn post_challenge(
    State(state): State<AppState>,
    body: Result<axum::Json<ChallengeRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(axum::Json(request)) = body else {
        return failure(
            StatusCode::BAD_REQUEST,
            code::INVALID_REQUEST,
            "challenge request body is invalid",
        );
    };
    let dir = state.auth.state_dir.clone();
    match tokio::task::spawn_blocking(move || challenge_blocking(&dir, request)).await {
        Ok(Ok(response)) => axum::Json(response).into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(e) => internal(format!("challenge join error: {e}")),
    }
}

fn challenge_blocking(
    dir: &StateDir,
    request: ChallengeRequest,
) -> Result<ChallengeResponse, AuthFailure> {
    let now_ms = hyoui::time::now_unix_ms();
    let rp = Rp::for_endpoint(&request.endpoint).map_err(|e| AuthFailure::bad(e.to_string()))?;
    let challenge_id = token::random_token();

    let (options, state_json, jti) = match request.purpose {
        ChallengePurpose::Assert => {
            let file: AuthFile = dir.auth().read().map_err(AuthFailure::store)?;
            let credentials: Vec<_> = file
                .live_credentials(&request.endpoint)
                .into_iter()
                .map(|record| record.credential.clone())
                .collect();
            if credentials.is_empty() {
                return Err(AuthFailure::denied(format!(
                    "no credential is registered for {}",
                    request.endpoint
                )));
            }
            let (challenge, auth_state) = rp
                .start_authentication(credentials)
                .map_err(|e| AuthFailure::denied(e.to_string()))?;
            (
                serde_json::to_value(&challenge.public_key).map_err(AuthFailure::encode)?,
                serde_json::to_value(&auth_state).map_err(AuthFailure::encode)?,
                None,
            )
        }
        ChallengePurpose::Register => {
            let jwt = request.jwt.as_deref().ok_or_else(|| {
                AuthFailure::denied("register challenge without a jwt".to_string())
            })?;
            let pending: PendingFile = dir.pending().read().map_err(AuthFailure::store)?;
            let jti = token::peek_jti(jwt).map_err(|e| AuthFailure::denied(e.to_string()))?;
            let registration = pending
                .registrations
                .get(&request.endpoint)
                .and_then(|by_jti| by_jti.get(&jti))
                .filter(|registration| registration.is_live(now_ms))
                .ok_or_else(|| AuthFailure::denied(format!("no live registration for {jti}")))?;
            let claims = token::verify_registration(jwt, &registration.hmac_secret, now_ms)
                .map_err(|e| AuthFailure::denied(e.to_string()))?;
            if claims.endpoint != request.endpoint {
                return Err(AuthFailure::denied(
                    "the jwt was issued for another endpoint".to_string(),
                ));
            }
            let (challenge, registration_state) = rp
                .start_registration(&registration.user_id, &registration.sub)
                .map_err(|e| AuthFailure::denied(e.to_string()))?;
            (
                serde_json::to_value(&challenge.public_key).map_err(AuthFailure::encode)?,
                serde_json::to_value(&registration_state).map_err(AuthFailure::encode)?,
                Some(jti),
            )
        }
    };

    let record = PendingChallenge {
        id: challenge_id.clone(),
        endpoint: request.endpoint.clone(),
        purpose: request.purpose,
        jti,
        state: state_json,
        expires_at_ms: now_ms + CHALLENGE_TTL_MS,
    };
    dir.pending()
        .update::<PendingFile, _, _>(|pending| {
            // 掃除は timer でなく読み書き時に行う (決定 5)。
            pending.sweep_expired(now_ms);
            pending
                .challenges
                .entry(request.endpoint.clone())
                .or_default()
                .insert(challenge_id.clone(), record);
        })
        .map_err(AuthFailure::store)?;

    Ok(ChallengeResponse {
        challenge_id,
        endpoint: request.endpoint,
        options,
    })
}

// -----------------------------------------------------------------------------
// POST /auth/register
// -----------------------------------------------------------------------------

async fn post_register(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<axum::Json<RegisterRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(axum::Json(request)) = body else {
        return failure(
            StatusCode::BAD_REQUEST,
            code::INVALID_REQUEST,
            "register request body is invalid",
        );
    };
    let user_agent = header_string(&headers, header::USER_AGENT);
    let dir = state.auth.state_dir.clone();
    match tokio::task::spawn_blocking(move || register_blocking(&dir, request, user_agent)).await {
        Ok(Ok(minted)) => minted.into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(e) => internal(format!("register join error: {e}")),
    }
}

/// 6 桁コードまで通った登録の材料 (= lock を手放してから WebAuthn 検証に渡す分)。
struct AcceptedRegistration {
    sub: String,
    user_id: Vec<u8>,
    access: super::record::Access,
    issued_label: Option<String>,
    jti: String,
    state: RegistrationState,
}

fn register_blocking(
    dir: &StateDir,
    request: RegisterRequest,
    user_agent: Option<String>,
) -> Result<MintedSession, AuthFailure> {
    let now_ms = hyoui::time::now_unix_ms();
    let rp = Rp::for_endpoint(&request.endpoint).map_err(|e| AuthFailure::bad(e.to_string()))?;
    let jti = token::peek_jti(&request.jwt).map_err(|e| AuthFailure::denied(e.to_string()))?;

    // (1) lock 下で jwt と 6 桁コードを見る。**試行回数の加算はここでしか起きない**
    // — lock の外で数えると 2 unit に来た要求で数え落とし、総当たりの回数上限が
    // unit の数だけ緩む (決定 4)。
    let accepted = dir
        .pending()
        .update::<PendingFile, _, _>(|pending| {
            pending.sweep_expired(now_ms);
            let by_jti = pending
                .registrations
                .get_mut(&request.endpoint)
                .ok_or_else(|| {
                    AuthFailure::denied("no registration for this endpoint".to_string())
                })?;
            let registration = by_jti
                .get_mut(&jti)
                .filter(|registration| registration.is_live(now_ms))
                .ok_or_else(|| AuthFailure::denied(format!("no live registration for {jti}")))?;
            token::verify_registration(&request.jwt, &registration.hmac_secret, now_ms)
                .map_err(|e| AuthFailure::denied(e.to_string()))
                .and_then(|claims| {
                    (claims.endpoint == request.endpoint)
                        .then_some(())
                        .ok_or_else(|| {
                            AuthFailure::denied(
                                "the jwt was issued for another endpoint".to_string(),
                            )
                        })
                })?;
            match registration.check_code(&request.code) {
                CodeOutcome::Accepted => {}
                CodeOutcome::Rejected { remaining } => {
                    return Err(AuthFailure::denied(format!(
                        "wrong code for {jti} ({remaining} attempts left)"
                    )));
                }
                CodeOutcome::Burned => {
                    // 上限に到達した URL は焼く (決定 2)。以後は再発行が要る。
                    by_jti.remove(&jti);
                    return Err(AuthFailure::denied(format!(
                        "registration {jti} is burned after too many wrong codes"
                    )));
                }
            }
            let registration = by_jti.get(&jti).expect("直前に触った登録");
            let (sub, user_id, access, issued_label) = (
                registration.sub.clone(),
                registration.user_id.clone(),
                registration.access,
                registration.issued_label.clone(),
            );
            // challenge は**消費せずに**読む (= 検証を通してから消す、決定 2)。
            let challenge = pending
                .challenge(
                    &request.endpoint,
                    &request.challenge_id,
                    ChallengePurpose::Register,
                    now_ms,
                )
                .filter(|challenge| challenge.jti.as_deref() == Some(jti.as_str()))
                .ok_or_else(|| {
                    AuthFailure::denied("no register challenge for this registration".to_string())
                })?;
            let state: RegistrationState = serde_json::from_value(challenge.state.clone())
                .map_err(|e| AuthFailure::denied(format!("stored registration state: {e}")))?;
            Ok(AcceptedRegistration {
                sub,
                user_id,
                access,
                issued_label,
                jti: jti.clone(),
                state,
            })
        })
        .map_err(AuthFailure::store)??;

    // (2) WebAuthn の登録検証と公開鍵の import。ここで落ちても jti は焼かない。
    let response: RegisterPublicKeyCredential = serde_json::from_value(request.credential)
        .map_err(|e| AuthFailure::denied(format!("create() response: {e}")))?;
    let credential = rp
        .finish_registration(&response, &accepted.state)
        .map_err(|e| AuthFailure::denied(e.to_string()))?;

    // (3) 通ったので record を置き、そのまま session を mint する (= 登録が即サインイン)。
    let record = CredentialRecord {
        sub: accepted.sub.clone(),
        user_id: accepted.user_id,
        access: accepted.access,
        sign_count: credential.counter,
        backup_eligible: credential.backup_eligible,
        backup_state: credential.backup_state,
        credential,
        issued_label: accepted.issued_label,
        device_label: request.device_label,
        registered_at_ms: now_ms,
        registered_user_agent: user_agent,
        last_used_at_ms: None,
        last_used_user_agent: None,
        tombstoned_at_ms: None,
    };
    let minted = dir
        .auth()
        .update::<AuthFile, _, _>(|file| {
            file.insert_credential(&request.endpoint, record);
            mint(file, &request.endpoint, &accepted.sub, now_ms)
        })
        .map_err(AuthFailure::store)?;

    // (4) 検証が通ってから jti と challenge を消費する (決定 2 の検証順序)。
    dir.pending()
        .update::<PendingFile, _, _>(|pending| {
            if let Some(by_jti) = pending.registrations.get_mut(&request.endpoint) {
                by_jti.remove(&accepted.jti);
            }
            pending.consume_challenge(
                &request.endpoint,
                &request.challenge_id,
                ChallengePurpose::Register,
                now_ms,
            );
            pending.sweep_expired(now_ms);
        })
        .map_err(AuthFailure::store)?;

    Ok(minted)
}

// -----------------------------------------------------------------------------
// POST /auth/assert
// -----------------------------------------------------------------------------

async fn post_assert(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<axum::Json<AssertRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(axum::Json(request)) = body else {
        return failure(
            StatusCode::BAD_REQUEST,
            code::INVALID_REQUEST,
            "assert request body is invalid",
        );
    };
    let user_agent = header_string(&headers, header::USER_AGENT);
    let dir = state.auth.state_dir.clone();
    match tokio::task::spawn_blocking(move || assert_blocking(&dir, request, user_agent)).await {
        Ok(Ok(minted)) => minted.into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(e) => internal(format!("assert join error: {e}")),
    }
}

fn assert_blocking(
    dir: &StateDir,
    request: AssertRequest,
    user_agent: Option<String>,
) -> Result<MintedSession, AuthFailure> {
    let now_ms = hyoui::time::now_unix_ms();
    let rp = Rp::for_endpoint(&request.endpoint).map_err(|e| AuthFailure::bad(e.to_string()))?;
    let response: PublicKeyCredential = serde_json::from_value(request.credential)
        .map_err(|e| AuthFailure::denied(format!("get() response: {e}")))?;

    // challenge は 1 回しか消費できない (決定 4)。lock 下で消す。
    let challenge = dir
        .pending()
        .update::<PendingFile, _, _>(|pending| {
            let consumed = pending.consume_challenge(
                &request.endpoint,
                &request.challenge_id,
                ChallengePurpose::Assert,
                now_ms,
            );
            pending.sweep_expired(now_ms);
            consumed
        })
        .map_err(AuthFailure::store)?
        .ok_or_else(|| AuthFailure::denied("no assert challenge with that id".to_string()))?;
    let state: AuthenticationState = serde_json::from_value(challenge.state)
        .map_err(|e| AuthFailure::denied(format!("stored authentication state: {e}")))?;

    dir.auth()
        .update::<AuthFile, _, _>(|file| {
            let presented = response.raw_id.clone();
            let record = file
                .find_live_credential_mut(&request.endpoint, &presented)
                .ok_or_else(|| AuthFailure::denied("no credential with that id".to_string()))?;
            let user_id = record.user_id.clone();
            let sub = record.sub.clone();
            // `userHandle` は crate が検証しないので hyoui 側で毎回照合する (決定 2)。
            let result = rp
                .finish_authentication(&response, &state, &user_id)
                .map_err(|e| AuthFailure::denied(e.to_string()))?;
            record.record_use(&result, now_ms, user_agent);
            Ok(mint(file, &request.endpoint, &sub, now_ms))
        })
        .map_err(AuthFailure::store)?
}

// -----------------------------------------------------------------------------
// POST /auth/refresh
// -----------------------------------------------------------------------------

async fn post_refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<axum::Json<RefreshRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(axum::Json(request)) = body else {
        return failure(
            StatusCode::BAD_REQUEST,
            code::INVALID_REQUEST,
            "refresh request body is invalid",
        );
    };
    let cookie = header_string(&headers, header::COOKIE).unwrap_or_default();
    let Some(presented) = token::refresh_from_cookie_header(&cookie, &request.endpoint) else {
        return AuthFailure::denied("no refresh cookie for this endpoint".to_string())
            .into_response();
    };
    let dir = state.auth.state_dir.clone();
    let endpoint = request.endpoint.clone();
    match tokio::task::spawn_blocking(move || refresh_blocking(&dir, &endpoint, &presented)).await {
        Ok(Ok(minted)) => minted.into_response(),
        // family を畳んだ時は cookie も落とす (= 次のリロードで即ログイン UI に落ちる)。
        Ok(Err(AuthFailure::Revoked)) => (
            StatusCode::UNAUTHORIZED,
            [(header::SET_COOKIE, token::clear_cookie(&request.endpoint))],
            axum::Json(crate::contract::ErrorEnvelope::from(ErrorInfo::new(
                code::AUTH_FAILED,
                "the authenticated session is no longer valid",
            ))),
        )
            .into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(e) => internal(format!("refresh join error: {e}")),
    }
}

fn refresh_blocking(
    dir: &StateDir,
    endpoint: &Endpoint,
    presented: &str,
) -> Result<MintedSession, AuthFailure> {
    let now_ms = hyoui::time::now_unix_ms();
    dir.auth()
        .update::<AuthFile, _, _>(|file| {
            let families = file
                .families
                .get_mut(endpoint)
                .ok_or_else(|| AuthFailure::denied("no family for this endpoint".to_string()))?;
            let (_, family) = families
                .iter_mut()
                .find(|(_, family)| {
                    family.classify_refresh(presented, now_ms) != RefreshOutcome::Unknown
                })
                .ok_or_else(|| AuthFailure::denied("no family holds that refresh".to_string()))?;
            // tombstone は「失効」— 新しい token を出さない (決定 4)。
            if family.is_tombstoned() {
                return Err(AuthFailure::Revoked);
            }
            match family.classify_refresh(presented, now_ms) {
                RefreshOutcome::Current => {
                    family.rotate_refresh(token::random_token(), token::random_token, now_ms);
                }
                // 直前 1 世代の猶予内の再送。rotate せず前回の答えを返す (決定 5)。
                RefreshOutcome::Replay => {}
                // どの世代かの再提示は family ごと失効させ、その sub の WS を切る。
                RefreshOutcome::Reused => {
                    family.tombstoned_at_ms = Some(now_ms);
                    return Err(AuthFailure::Revoked);
                }
                RefreshOutcome::Unknown => unreachable!("Unknown は上で弾いている"),
            }
            if !family.refresh.is_live(now_ms) || !family.access.is_live(now_ms) {
                return Err(AuthFailure::Revoked);
            }
            Ok(MintedSession::from_family(family))
        })
        .map_err(AuthFailure::store)?
}

// -----------------------------------------------------------------------------
// 応答
// -----------------------------------------------------------------------------

/// 認証が通った時の応答 (= access は body、refresh は cookie だけ、決定 5)。
struct MintedSession {
    endpoint: Endpoint,
    sub: String,
    access: String,
    access_expires_at_ms: u64,
    refresh: String,
}

impl MintedSession {
    fn from_family(family: &FamilyRecord) -> Self {
        Self {
            endpoint: family.endpoint.clone(),
            sub: family.sub.clone(),
            access: family.access.value.clone(),
            access_expires_at_ms: family.access.expires_at_ms,
            refresh: family.refresh.value.clone(),
        }
    }
}

impl IntoResponse for MintedSession {
    fn into_response(self) -> Response {
        let body = SessionResponse {
            access_token: self.access,
            expires_at: hyoui::time::format_unix_ms_iso8601(self.access_expires_at_ms),
            sub: self.sub,
        };
        (
            StatusCode::OK,
            [(
                header::SET_COOKIE,
                token::set_cookie(&self.endpoint, &self.refresh, REFRESH_TTL_MS / 1000),
            )],
            axum::Json(body),
        )
            .into_response()
    }
}

/// family を 1 本新しく mint する。
fn mint(file: &mut AuthFile, endpoint: &Endpoint, sub: &str, now_ms: u64) -> MintedSession {
    let family = file.mint_family(
        endpoint,
        sub,
        token::random_token(),
        token::random_token(),
        token::random_token(),
        now_ms,
    );
    MintedSession::from_family(&family)
}

/// `/auth/*` の失敗。
///
/// **`Denied` は理由を応答に出さない** (決定 5)。`String` は log 用である。
enum AuthFailure {
    /// 要求の形が不正 (= endpoint が RP として使えない等)。400。
    Bad(String),
    /// 検証が通らなかった。401、`code: auth-failed`。
    Denied(String),
    /// family が失効している。401 + cookie を落とす。
    Revoked,
    /// gateway 内部の失敗 (= state file が読めない)。500。
    Internal(String),
}

impl AuthFailure {
    fn bad(reason: String) -> Self {
        AuthFailure::Bad(reason)
    }

    fn denied(reason: String) -> Self {
        AuthFailure::Denied(reason)
    }

    fn store(error: super::store::StoreError) -> Self {
        AuthFailure::Internal(error.to_string())
    }

    fn encode(error: serde_json::Error) -> Self {
        AuthFailure::Internal(format!("encode challenge: {error}"))
    }
}

impl IntoResponse for AuthFailure {
    fn into_response(self) -> Response {
        match self {
            AuthFailure::Bad(reason) => failure(
                StatusCode::BAD_REQUEST,
                code::INVALID_REQUEST,
                reason.as_str(),
            ),
            AuthFailure::Denied(reason) => {
                // 切り分けは log でできる。応答では理由を分けない (決定 5)。
                eprintln!("hyoui-web: authentication failed: {reason}");
                failure(
                    StatusCode::UNAUTHORIZED,
                    code::AUTH_FAILED,
                    "authentication failed",
                )
            }
            AuthFailure::Revoked => failure(
                StatusCode::UNAUTHORIZED,
                code::AUTH_FAILED,
                "the authenticated session is no longer valid",
            ),
            AuthFailure::Internal(reason) => internal(reason),
        }
    }
}

fn failure(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        axum::Json(crate::contract::ErrorEnvelope::from(ErrorInfo::new(
            code, message,
        ))),
    )
        .into_response()
}

fn internal(reason: String) -> Response {
    eprintln!("hyoui-web: {reason}");
    failure(
        StatusCode::INTERNAL_SERVER_ERROR,
        code::INTERNAL_ERROR,
        "internal error",
    )
}

fn header_string(headers: &HeaderMap, name: header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string())
}

// -----------------------------------------------------------------------------
// WS の認証 (決定 5)
// -----------------------------------------------------------------------------

/// 確立済み WS が `auth.extend` で提示した token を検証する土台。
///
/// **family を file から読み直す。** これが失効を確立済み接続に反映する唯一の点
/// である (決定 4 / 決定 5)。
#[derive(Clone)]
pub struct WsAuth {
    dir: Arc<StateDir>,
    /// hello に載せる access の期限 (unix ms)。
    pub expires_at_ms: u64,
}

impl WsAuth {
    pub(crate) fn new(dir: Arc<StateDir>, expires_at_ms: u64) -> Self {
        Self { dir, expires_at_ms }
    }

    /// 提示された access token でこの接続の期限を延ばす。
    ///
    /// 返すのは延長後の期限 (ISO 8601)。`None` は「その family は失効した」で、
    /// 呼び出し側は接続を閉じる。
    pub fn extend(&self, presented: &str) -> Option<String> {
        let now_ms = hyoui::time::now_unix_ms();
        let file: AuthFile = match self.dir.auth().read() {
            Ok(file) => file,
            Err(e) => {
                eprintln!("hyoui-web: auth.json read failed during auth.extend: {e}");
                return None;
            }
        };
        let family = file.find_live_family_by_access(presented, now_ms)?;
        Some(hyoui::time::format_unix_ms_iso8601(
            family.access.expires_at_ms,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fixed_window_admits_the_limit_and_refuses_the_rest() {
        let mut limiter = RateLimiter::new(3);
        assert!(limiter.allow());
        assert!(limiter.allow());
        assert!(limiter.allow());
        assert!(!limiter.allow(), "4 本目は落とす");
        // 窓が進めば再び通る。
        limiter.window_started = Instant::now() - Duration::from_secs(2);
        assert!(limiter.allow());
    }

    #[test]
    fn the_token_is_read_from_either_bearer_or_the_ws_subprotocol() {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer abc".parse().unwrap());
        assert_eq!(presented_token(&headers), Some("abc".to_string()));

        let mut headers = HeaderMap::new();
        headers.insert(
            "sec-websocket-protocol",
            "hyoui.token.xyz123".parse().unwrap(),
        );
        assert_eq!(presented_token(&headers), Some("xyz123".to_string()));
        assert_eq!(
            ws_token_protocol(&headers),
            Some(("hyoui.token.xyz123".to_string(), "xyz123".to_string())),
            "選んだ subprotocol はそのまま echo するので元の文字列も要る"
        );

        // 複数提案されても hyoui の 1 本だけを拾う。
        let mut headers = HeaderMap::new();
        headers.insert(
            "sec-websocket-protocol",
            "other, hyoui.token.tok, another".parse().unwrap(),
        );
        assert_eq!(presented_token(&headers), Some("tok".to_string()));

        // token が空、prefix 違い、ヘッダ無しはいずれも「無い」。
        for value in ["hyoui.token.", "hyoui.tok", ""] {
            let mut headers = HeaderMap::new();
            if !value.is_empty() {
                headers.insert("sec-websocket-protocol", value.parse().unwrap());
            }
            assert_eq!(presented_token(&headers), None, "{value:?}");
        }
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer ".parse().unwrap());
        assert_eq!(
            presented_token(&headers),
            None,
            "空の Bearer は token でない"
        );
    }
}
