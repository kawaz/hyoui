//! 確立済み WS 1 本の認証 (DR-0036 決定 4 / 決定 5)。
//!
//! 接続は **自分を開いた family** と **現在の access の期限** を持つ。切れるのは次の
//! 3 つの時だけである:
//!
//! - 期限までに `auth.extend` で延ばさなかった (= 延長を怠った接続だけを切る)
//! - `auth.extend` が拒まれた (別 family / 期限切れ / 不明の access、または family の失効)
//! - 同じプロセスの `/auth/refresh` が再利用を検知して family を失効させた
//!
//! **CLI による失効 (`passkey remove` / `session remove`) は gateway に通知されない**
//! (決定 4)。それが確立済み接続に届くのは次の `auth.extend` か期限で、期限で必ず
//! 切ることが「最長 access TTL」の上限を成り立たせる。
//!
//! 期限の計時は tokio の時計で行う (= test は時計を止めて進められる)。

use std::sync::Arc;

use tokio::sync::watch;
use tokio::time::Instant;

use super::record::AuthFile;
use super::routes::Identity;
use super::store::StateDir;
use crate::contract::Endpoint;

/// 確立済み接続の認証が終わった理由。どれも close code は
/// [`crate::contract::ws_close::AUTH_ENDED`] で、違うのは close reason の文言だけ
/// (= browser の扱いは同じ。理由は log と開発者向け)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthLoss {
    /// 期限までに延長されなかった。
    Expired,
    /// 接続を開いた family が失効した。
    Revoked,
    /// `auth.extend` の access が、この接続の family の現行の値ではなかった。
    Rejected,
}

impl AuthLoss {
    /// close frame の reason (RFC 6455 の上限 123 byte に収まる短い文)。
    pub fn close_reason(self) -> &'static str {
        match self {
            AuthLoss::Expired => "access expired",
            AuthLoss::Revoked => "access revoked",
            AuthLoss::Rejected => "access rejected",
        }
    }
}

/// 同じプロセス内の確立済み WS へ「family が失効した」を知らせる経路。
///
/// 値は世代番号で、中身に意味は無い。受けた接続は file を読み直して**自分の**
/// family が失効したかを確かめる (= どの family かを message に載せない。取りこぼしても
/// 読み直しで追いつく)。
#[derive(Clone)]
pub(crate) struct RevocationSignal(Arc<watch::Sender<u64>>);

impl RevocationSignal {
    pub(crate) fn new() -> Self {
        Self(Arc::new(watch::Sender::new(0)))
    }

    /// family を失効させた (= `auth.json` に書いた) 後に呼ぶ。
    pub(crate) fn notify(&self) {
        self.0
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    fn subscribe(&self) -> watch::Receiver<u64> {
        self.0.subscribe()
    }
}

/// 確立済み WS 1 本の認証。
pub(crate) struct WsAuth {
    dir: Arc<StateDir>,
    endpoint: Endpoint,
    family_id: String,
    expires_at_ms: u64,
    /// `expires_at_ms` (unix ms) を tokio の時計に写す基準点。
    anchor: (Instant, u64),
    revocations: watch::Receiver<u64>,
}

impl WsAuth {
    pub(crate) fn new(
        dir: Arc<StateDir>,
        revocations: &RevocationSignal,
        identity: &Identity,
        now_ms: u64,
    ) -> Self {
        let mut revocations = revocations.subscribe();
        // middleware が family を読んでから購読するまでの間に失効が来ていても
        // 取りこぼさないよう、最初の待ちで 1 度 file を読み直させる。
        revocations.mark_changed();
        Self {
            dir,
            endpoint: identity.endpoint.clone(),
            family_id: identity.family_id.clone(),
            expires_at_ms: identity.access_expires_at_ms,
            anchor: (Instant::now(), now_ms),
            revocations,
        }
    }

    /// 現在の access の期限 (unix ms)。
    pub fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }

    /// `auth.extend` で提示された access でこの接続の期限を延ばす。
    ///
    /// 延ばせるのは **この接続を開いた family の現行 access** だけである。返すのは
    /// 延長後の期限 (unix ms)。`Err` なら呼び出し側は接続を閉じる。
    pub fn extend(&mut self, presented: &str) -> Result<u64, AuthLoss> {
        self.extend_at(presented, hyoui::time::now_unix_ms())
    }

    pub(crate) fn extend_at(&mut self, presented: &str, now_ms: u64) -> Result<u64, AuthLoss> {
        // family は cache せず毎回 file から読む (決定 4)。
        let file: AuthFile = self.dir.auth().read().map_err(|e| {
            eprintln!("hyoui-web: auth.json read failed during auth.extend: {e}");
            AuthLoss::Rejected
        })?;
        let family = match file.family(&self.endpoint, &self.family_id) {
            Some(family) if !family.is_tombstoned() => family,
            _ => return Err(AuthLoss::Revoked),
        };
        if !family.holds_live_access(presented, now_ms) {
            return Err(AuthLoss::Rejected);
        }
        self.expires_at_ms = family.access.expires_at_ms;
        Ok(self.expires_at_ms)
    }

    /// この接続の認証が終わるまで待つ。
    ///
    /// 何度呼んでもよく、途中で落としてもよい (= `select!` の 1 枝に置く)。期限は
    /// 呼ぶたびに今の `expires_at_ms` から計算し直すので、`extend` の後は新しい期限で待つ。
    pub async fn lost(&mut self) -> AuthLoss {
        let deadline = self.deadline();
        loop {
            tokio::select! {
                () = tokio::time::sleep_until(deadline) => return AuthLoss::Expired,
                changed = self.revocations.changed() => {
                    if changed.is_err() {
                        // 知らせる側が居なくなった。残る終わり方は期限だけ。
                        tokio::time::sleep_until(deadline).await;
                        return AuthLoss::Expired;
                    }
                    if family_is_revoked(&self.dir, &self.endpoint, &self.family_id) {
                        return AuthLoss::Revoked;
                    }
                }
            }
        }
    }

    fn deadline(&self) -> Instant {
        let (instant, ms) = self.anchor;
        instant + std::time::Duration::from_millis(self.expires_at_ms.saturating_sub(ms))
    }
}

/// family が失効したか (= tombstone か、record ごと無いか)。
///
/// **読めない時は失効扱いにする。** 読めない状態では新規の `/api/*` も通らないので、
/// 確立済み接続だけを生かしておく理由が無い。
fn family_is_revoked(dir: &StateDir, endpoint: &Endpoint, family_id: &str) -> bool {
    match dir.auth().read::<AuthFile>() {
        Ok(file) => file
            .family(endpoint, family_id)
            .is_none_or(|family| family.is_tombstoned()),
        Err(e) => {
            eprintln!("hyoui-web: auth.json read failed while watching a WS: {e}");
            true
        }
    }
}

#[cfg(test)]
mod tests {
    //! 期限は tokio の時計を止めて進める (= 実時間を待たない)。

    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt;

    use super::*;
    use crate::auth::record::ACCESS_TTL_MS;
    use crate::auth::{AuthContext, token};

    struct Fixture {
        context: AuthContext,
        endpoint: Endpoint,
        _state: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let state = tempfile::tempdir().expect("tempdir");
            Self {
                context: AuthContext::at(StateDir::at(state.path())),
                endpoint: Endpoint::parse("http://127.0.0.1:43690/").unwrap(),
                _state: state,
            }
        }

        /// family を 1 本置き、(access, refresh) を返す。
        fn seed(&self, family_id: &str, now_ms: u64) -> (String, String) {
            let access = token::random_token();
            let refresh = token::random_token();
            self.context
                .state_dir()
                .auth()
                .update::<AuthFile, _, _>(|file| {
                    file.mint_family(
                        &self.endpoint,
                        "sub-1",
                        family_id.to_string(),
                        access.clone(),
                        refresh.clone(),
                        now_ms,
                    );
                })
                .expect("family を置く");
            (access, refresh)
        }

        fn identity(&self, family_id: &str, access_expires_at_ms: u64) -> Identity {
            Identity {
                sub: "sub-1".to_string(),
                endpoint: self.endpoint.clone(),
                family_id: family_id.to_string(),
                access_expires_at_ms,
            }
        }

        fn tombstone(&self, family_id: &str) {
            self.context
                .state_dir()
                .auth()
                .update::<AuthFile, _, _>(|file| {
                    let family = file
                        .families
                        .get_mut(&self.endpoint)
                        .and_then(|by_id| by_id.get_mut(family_id))
                        .expect("family");
                    family.tombstoned_at_ms = Some(1);
                })
                .expect("tombstone");
        }
    }

    /// `lost()` が `within` の間に終わらないこと。
    async fn still_alive(auth: &mut WsAuth, within: Duration) {
        let outcome = tokio::time::timeout(within, auth.lost()).await;
        assert!(outcome.is_err(), "まだ切れない: {outcome:?}");
    }

    /// 延長しなかった接続は期限ちょうどで切れる (決定 5)。
    #[tokio::test(start_paused = true)]
    async fn an_unextended_connection_ends_at_the_access_deadline() {
        let fixture = Fixture::new();
        let now_ms = 1_000_000;
        fixture.seed("fam-a", now_ms);
        let mut auth = fixture
            .context
            .ws_auth_at(&fixture.identity("fam-a", now_ms + ACCESS_TTL_MS), now_ms);

        still_alive(&mut auth, Duration::from_millis(ACCESS_TTL_MS - 1)).await;
        let outcome = tokio::time::timeout(Duration::from_millis(2), auth.lost()).await;
        assert_eq!(outcome, Ok(AuthLoss::Expired));
    }

    /// `auth.extend` で延ばした接続は、延ばした後の期限まで切れない。
    #[tokio::test(start_paused = true)]
    async fn extending_moves_the_deadline() {
        let fixture = Fixture::new();
        let now_ms = 1_000_000;
        let (access, _) = fixture.seed("fam-a", now_ms);
        // 接続を開いた時の access は 1 秒で切れる値だった、とする。
        let mut auth = fixture
            .context
            .ws_auth_at(&fixture.identity("fam-a", now_ms + 1_000), now_ms);

        still_alive(&mut auth, Duration::from_millis(500)).await;
        assert_eq!(
            auth.extend_at(&access, now_ms + 500),
            Ok(now_ms + ACCESS_TTL_MS)
        );
        // 元の期限 (1 秒) を過ぎても切れない。
        still_alive(&mut auth, Duration::from_millis(ACCESS_TTL_MS - 501)).await;
        let outcome = tokio::time::timeout(Duration::from_millis(2), auth.lost()).await;
        assert_eq!(outcome, Ok(AuthLoss::Expired));
    }

    /// 延ばせるのはその接続を開いた family の現行 access だけ (決定 5)。
    #[tokio::test(start_paused = true)]
    async fn extend_accepts_only_the_access_of_the_family_that_opened_the_connection() {
        let fixture = Fixture::new();
        let now_ms = 1_000_000;
        let (access_a, _) = fixture.seed("fam-a", now_ms);
        let (access_b, _) = fixture.seed("fam-b", now_ms);
        let opened_until = now_ms + 1_000;
        let mut auth = fixture
            .context
            .ws_auth_at(&fixture.identity("fam-a", opened_until), now_ms);

        // 別 family の有効な access は通らない。期限も動かない。
        assert_eq!(auth.extend_at(&access_b, now_ms), Err(AuthLoss::Rejected));
        assert_eq!(auth.expires_at_ms(), opened_until);
        // 不明の値も、期限を過ぎた自分の access も通らない。
        assert_eq!(
            auth.extend_at("not-a-token", now_ms),
            Err(AuthLoss::Rejected)
        );
        assert_eq!(
            auth.extend_at(&access_a, now_ms + ACCESS_TTL_MS),
            Err(AuthLoss::Rejected)
        );
        // 自分の access なら通る。
        assert_eq!(
            auth.extend_at(&access_a, now_ms),
            Ok(now_ms + ACCESS_TTL_MS)
        );
        // family が失効したら自分の access でも通らない。
        fixture.tombstone("fam-a");
        assert_eq!(auth.extend_at(&access_a, now_ms), Err(AuthLoss::Revoked));
    }

    /// refresh の再利用検知はその family の確立済み WS を切り、他の family には触れない
    /// (決定 5)。
    #[tokio::test(start_paused = true)]
    async fn refresh_reuse_ends_the_connections_of_that_family_only() {
        let fixture = Fixture::new();
        // `/auth/refresh` は実時刻で判定するので、family も実時刻で置く。
        let now_ms = hyoui::time::now_unix_ms();
        let (_, refresh_a) = fixture.seed("fam-a", now_ms);
        fixture.seed("fam-b", now_ms);
        let expires = now_ms + ACCESS_TTL_MS;
        let mut auth_a = fixture
            .context
            .ws_auth_at(&fixture.identity("fam-a", expires), now_ms);
        let mut auth_b = fixture
            .context
            .ws_auth_at(&fixture.identity("fam-b", expires), now_ms);
        still_alive(&mut auth_a, Duration::from_secs(1)).await;
        still_alive(&mut auth_b, Duration::from_secs(1)).await;

        let app = crate::router_with_auth(
            hyoui::config::Config::default(),
            None,
            fixture.context.clone(),
        );
        let refresh = |value: &str| {
            Request::builder()
                .method("POST")
                .uri("/auth/refresh")
                .header(header::CONTENT_TYPE, "application/json")
                .header(
                    header::COOKIE,
                    format!("{}={value}", token::cookie_name(&fixture.endpoint)),
                )
                .body(Body::from(
                    serde_json::json!({"endpoint": fixture.endpoint.as_str()}).to_string(),
                ))
                .unwrap()
        };
        // 1 度 rotate し、猶予を潰してから旧い値を出す (= 再利用)。
        let response = app.clone().oneshot(refresh(&refresh_a)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        fixture
            .context
            .state_dir()
            .auth()
            .update::<AuthFile, _, _>(|file| {
                for family in file.families.values_mut().flat_map(|f| f.values_mut()) {
                    for retired in &mut family.retired {
                        retired.retired_at_ms = 0;
                    }
                }
            })
            .expect("猶予を潰す");
        let response = app.oneshot(refresh(&refresh_a)).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // 期限 (4 時間) を待たずに、その family の接続だけが切れる。
        let outcome = tokio::time::timeout(Duration::from_secs(1), auth_a.lost()).await;
        assert_eq!(outcome, Ok(AuthLoss::Revoked));
        still_alive(&mut auth_b, Duration::from_secs(1)).await;
    }

    /// 接続を張る前に失効していた family は、最初の待ちで切れる (= 購読前の失効を
    /// 取りこぼさない)。
    #[tokio::test(start_paused = true)]
    async fn a_family_revoked_before_the_watch_starts_is_not_missed() {
        let fixture = Fixture::new();
        let now_ms = 1_000_000;
        fixture.seed("fam-a", now_ms);
        fixture.tombstone("fam-a");
        let mut auth = fixture
            .context
            .ws_auth_at(&fixture.identity("fam-a", now_ms + ACCESS_TTL_MS), now_ms);
        let outcome = tokio::time::timeout(Duration::from_millis(1), auth.lost()).await;
        assert_eq!(outcome, Ok(AuthLoss::Revoked));
    }
}
