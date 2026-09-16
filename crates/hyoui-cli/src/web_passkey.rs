//! `hyoui web passkey` / `hyoui web session` (DR-0036 決定 2 / 決定 4 / 決定 5)。
//!
//! **どのコマンドも gateway に要求を送らない。** CLI が
//! `$XDG_STATE_HOME/hyoui-web/{auth,pending}.json` を直に読み書きし、gateway は
//! 読むだけである (決定 2 / 決定 4)。これで 2 つのことが同時に成り立つ:
//!
//! - **gateway に管理用の経路を足さずに済む。** 登録 URL の発行に gateway の生存が
//!   要らないので、`hyoui web daemon` が全て停止していても `add` が打てる
//! - **secret がプロセスのメモリに無いので、どの unit が POST を受けても検証できる。**
//!   HA endpoint 宛の登録で「発行した unit」と「受ける unit」が違いうることを、
//!   file を正本にすることで気にしなくてよくなる
//!
//! 到達が権限である: CLI は同じホスト上で同じ uid で走るので state dir に書ける。
//! ccmsg が UDS の管理フレームで表していることを、hyoui では file の所有権で表す。
//!
//! **失効は削除ではなく tombstone で表す** (決定 4)。`remove` は record を消さずに
//! 畳むので、後から「いつ何を消したか」が読める。

use std::process::ExitCode;

use hyoui::cli::{WebPasskeyAddConfig, WebPasskeyCommand, WebSessionCommand};
use hyoui_web::auth::token;
use hyoui_web::auth::{AuthFile, PendingFile, PendingRegistration, REGISTRATION_TTL_MS, StateDir};
use hyoui_web::contract::Endpoint;
use serde_json::{Value, json};

/// `hyoui web passkey <subcommand>` を実行する。
pub fn passkey_command(command: WebPasskeyCommand) -> ExitCode {
    match command {
        WebPasskeyCommand::Add(config) => add(config),
        WebPasskeyCommand::List => list(),
        WebPasskeyCommand::Remove { sub } => remove(&sub),
        // 版のずれ (= 新しい parser + 古い binary) を黙って成功にしない。
        _ => fail(
            "web passkey",
            "unsupported passkey subcommand variant (binary/library version skew)",
            None,
        ),
    }
}

/// `hyoui web session <subcommand>` を実行する。
pub fn session_command(command: WebSessionCommand) -> ExitCode {
    match command {
        WebSessionCommand::List => session_list(),
        WebSessionCommand::Remove { id } => session_remove(&id),
        _ => fail(
            "web session",
            "unsupported session subcommand variant (binary/library version skew)",
            None,
        ),
    }
}

// -----------------------------------------------------------------------------
// passkey add
// -----------------------------------------------------------------------------

/// 招待 URL と 6 桁コードを発行する (決定 2)。
///
/// 出すのは `<endpoint>#register=<jwt>` と 6 桁コードの 2 つ。**jwt は fragment に
/// 載せる** ので server にも proxy log にも Referer にも乗らない。コードは URL に
/// 含めず、ここにだけ表示する (= URL が漏れただけでは登録にならない)。
fn add(config: WebPasskeyAddConfig) -> ExitCode {
    let endpoint = match Endpoint::parse(&config.endpoint) {
        Ok(endpoint) => endpoint,
        // 正規化できない値は拒否する (決定 3) — 1 文字違う key で書くと、
        // ブラウザが送る正規形では引けず、原因が認証失敗としてしか見えない。
        Err(e) => {
            return fail(
                "web passkey add",
                &format!("--endpoint {:?} cannot be used: {e}", config.endpoint),
                Some(json!({"hint": "use a full URL such as https://hyoui.example.jp/"})),
            );
        }
    };

    let now_ms = hyoui::time::now_unix_ms();
    let dir = StateDir::default_root();
    let existing: AuthFile = match dir.auth().read() {
        Ok(file) => file,
        Err(e) => return fail("web passkey add", &e.to_string(), None),
    };
    // `sub` の既定は `<endpoint の host>-<連番>` で、**unit に依存させない** (決定 2)。
    let sub = existing.next_sub(&endpoint);
    let user_id = token::random_user_id();
    let secret = token::random_secret();
    let code = token::random_code();
    let jti = token::random_token();
    let expires_at_ms = now_ms + REGISTRATION_TTL_MS;

    let claims = token::RegistrationClaims {
        sub: sub.clone(),
        endpoint: endpoint.clone(),
        rp_id: endpoint.rp_id().to_string(),
        user_id: token::base64url(&user_id),
        access: Default::default(),
        exp: expires_at_ms / 1000,
        jti: jti.clone(),
    };
    let jwt = token::encode_registration(&claims, &secret);

    let registration = PendingRegistration {
        jti: jti.clone(),
        sub: sub.clone(),
        user_id,
        access: Default::default(),
        hmac_secret: secret,
        code_hash: token::code_hash(&code),
        code_attempts: 0,
        issued_label: config.label.clone(),
        expires_at_ms,
    };
    if let Err(e) = dir.pending().update::<PendingFile, _, _>(|pending| {
        // 掃除は timer でなく読み書き時に行う (決定 5)。
        pending.sweep_expired(now_ms);
        pending
            .registrations
            .entry(endpoint.clone())
            .or_default()
            .insert(jti.clone(), registration);
    }) {
        return fail("web passkey add", &e.to_string(), None);
    }

    emit(&json!({
        "endpoint": endpoint.as_str(),
        "sub": sub,
        "label": config.label,
        "invite_url": format!("{endpoint}#register={jwt}"),
        "code": code,
        "expires_at": hyoui::time::format_unix_ms_iso8601(expires_at_ms),
        "next": "open the invite URL on the device, then type the code shown here",
    }))
}

// -----------------------------------------------------------------------------
// passkey list / remove
// -----------------------------------------------------------------------------

fn list() -> ExitCode {
    let dir = StateDir::default_root();
    let file: AuthFile = match dir.auth().read() {
        Ok(file) => file,
        Err(e) => return fail("web passkey list", &e.to_string(), None),
    };
    let mut passkeys = Vec::new();
    for (endpoint, subs) in &file.credentials {
        for records in subs.values() {
            for record in records.values() {
                passkeys.push(json!({
                    "endpoint": endpoint.as_str(),
                    "sub": record.sub,
                    "access": record.access.as_str(),
                    "issued_label": record.issued_label,
                    "device_label": record.device_label,
                    "registered_at": hyoui::time::format_unix_ms_iso8601(record.registered_at_ms),
                    "registered_user_agent": record.registered_user_agent,
                    "last_used_at": record
                        .last_used_at_ms
                        .map(hyoui::time::format_unix_ms_iso8601),
                    "last_used_user_agent": record.last_used_user_agent,
                    "sign_count": record.sign_count,
                    "backup_eligible": record.backup_eligible,
                    "backup_state": record.backup_state,
                    "revoked_at": record
                        .tombstoned_at_ms
                        .map(hyoui::time::format_unix_ms_iso8601),
                }));
            }
        }
    }
    let mut output = json!({"passkeys": passkeys, "state_dir": dir.root()});
    if output["passkeys"].as_array().is_some_and(Vec::is_empty) {
        // 空は「何も無い」で終わらせず、次に取れる操作を出す。
        output["next"] = json!("register a device with `hyoui web passkey add --endpoint <url>`");
    }
    emit(&output)
}

fn remove(sub: &str) -> ExitCode {
    let dir = StateDir::default_root();
    let now_ms = hyoui::time::now_unix_ms();
    let outcome = dir
        .auth()
        .update::<AuthFile, _, _>(|file| file.tombstone_sub(sub, now_ms));
    match outcome {
        Ok((0, 0)) => fail(
            "web passkey remove",
            &format!("no live passkey named {sub:?}"),
            Some(json!({"hint": "`hyoui web passkey list` prints the registered names"})),
        ),
        Ok((credentials, families)) => emit(&json!({
            "sub": sub,
            "revoked_credentials": credentials,
            "revoked_sessions": families,
            "revoked_at": hyoui::time::format_unix_ms_iso8601(now_ms),
            // 確立済みの WS は次の `auth.extend` まで生き延びる (決定 4)。
            "note": "an open WebSocket is cut when it next extends its session (at most 4 hours); \
                     `hyoui web daemon restart --all` cuts every connection now",
        })),
        Err(e) => fail("web passkey remove", &e.to_string(), None),
    }
}

// -----------------------------------------------------------------------------
// session list / remove
// -----------------------------------------------------------------------------

fn session_list() -> ExitCode {
    let dir = StateDir::default_root();
    let file: AuthFile = match dir.auth().read() {
        Ok(file) => file,
        Err(e) => return fail("web session list", &e.to_string(), None),
    };
    let mut sessions = Vec::new();
    for (endpoint, families) in &file.families {
        for family in families.values() {
            sessions.push(json!({
                "id": family.id,
                "endpoint": endpoint.as_str(),
                "sub": family.sub,
                "access_expires_at": hyoui::time::format_unix_ms_iso8601(
                    family.access.expires_at_ms,
                ),
                "refresh_expires_at": hyoui::time::format_unix_ms_iso8601(
                    family.refresh.expires_at_ms,
                ),
                "retired_generations": family.retired.len(),
                "revoked_at": family
                    .tombstoned_at_ms
                    .map(hyoui::time::format_unix_ms_iso8601),
            }));
        }
    }
    emit(&json!({"sessions": sessions, "state_dir": dir.root()}))
}

fn session_remove(id: &str) -> ExitCode {
    let dir = StateDir::default_root();
    let now_ms = hyoui::time::now_unix_ms();
    let outcome = dir.auth().update::<AuthFile, _, _>(|file| {
        let mut revoked = 0;
        for families in file.families.values_mut() {
            if let Some(family) = families.get_mut(id)
                && !family.is_tombstoned()
            {
                family.tombstoned_at_ms = Some(now_ms);
                revoked += 1;
            }
        }
        revoked
    });
    match outcome {
        Ok(0) => fail(
            "web session remove",
            &format!("no live session with id {id:?}"),
            Some(json!({"hint": "`hyoui web session list` prints the ids"})),
        ),
        Ok(revoked) => emit(&json!({
            "id": id,
            "revoked_sessions": revoked,
            "revoked_at": hyoui::time::format_unix_ms_iso8601(now_ms),
            "note": "an open WebSocket is cut when it next extends its session (at most 4 hours)",
        })),
        Err(e) => fail("web session remove", &e.to_string(), None),
    }
}

// -----------------------------------------------------------------------------
// 出力
// -----------------------------------------------------------------------------

fn emit(value: &Value) -> ExitCode {
    match serde_json::to_string_pretty(value) {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("hyoui: could not serialize output: {error}");
            ExitCode::from(1)
        }
    }
}

fn fail(context: &str, message: &str, details: Option<Value>) -> ExitCode {
    let mut error = json!({"command": context, "error": message});
    if let Some(Value::Object(details)) = details {
        for (key, value) in details {
            error[key] = value;
        }
    }
    match serde_json::to_string_pretty(&error) {
        Ok(text) => eprintln!("{text}"),
        Err(_) => eprintln!("hyoui: {context}: {message}"),
    }
    ExitCode::from(1)
}
