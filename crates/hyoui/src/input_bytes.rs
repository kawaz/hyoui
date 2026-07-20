//! `hyoui input <session> <spec>...` の各 spec prefix handler。
//!
//! DR-0006 §8 で定義された input family の各 prefix を bytes 列に変換する。
//! 変換結果の bytes は caller (= `main.rs::input_command`) が
//! [`hyoui::client::ClientConnection::send_raw_bytes`] で daemon に流す。
//!
//! 採用方針 (= 既存 raw bytes 経路):
//! - daemon 側に新規 control message を追加せず、既存の `TYPE_RAW_DATA` frame に
//!   bytes を載せて流す
//! - daemon は受け取った raw_data frame の body を master PTY にそのまま書く
//!   (= `daemon::control::handle_client_frame` の `TYPE_RAW_DATA` 分岐)
//! - したがって handler の責務は「spec を bytes に正規化する」ことに限定される
//!
//! 例外 (= 別 task で実装):
//! - `wait:` / `wait-idle:` は本 module の対象外 (= task #17、別 control message)
//!
//! ## task #21 (= file: spec の path validation / セキュリティ視点)
//!
//! 本 module の [`handle_file`] は task #21 でセキュリティ視点の防御を追加した。
//! 脅威モデルと採用した防御方針:
//!
//! - daemon 側は client が送る bytes をそのまま PTY に流すだけで、file path
//!   そのものを daemon に渡さない (= path 解釈は **client 側** で完結する)
//! - したがって elevated 権限の daemon に任意 path を読ませる経路は最初から
//!   無い。本 task の脅威モデルは下記 4 つに絞った:
//!   1. 巨大 file 誤指定 → daemon に超大 bytes を送って memory 枯渇 DoS
//!   2. typo で sensitive file (= `/etc/passwd` 等) を誤って送信
//!   3. symlink traversal による意図しない path 到達
//!   4. device file (= `/dev/zero` 等) を読んで無限 loop
//!
//! 採用した防御:
//!
//! - **size 上限**: default 16 MiB、`--max-file-bytes` / `HYOUI_MAX_FILE_BYTES`
//!   で override。metadata で事前判定 (= 巨大 file を読み始めてから止める動きを
//!   避ける)。symlink follow した先の size を見る (= `fs::metadata`)
//! - **regular file 限定**: directory / device / socket / fifo は reject。
//!   symlink は **follow した結果が regular file ならば accept** (= 安全側)。
//!   `stdin` (= `-`) は file type check の対象外 (= pipe / tty を許す)
//! - **空 file の warning**: bytes を 1 byte も送らないので意図に反する可能性が
//!   ある。stderr に warning を出して **続行** (= UX 優先、abort はしない)
//!
//! 採用見送り (= 別 task で検討):
//!
//! - sensitive path (= `/etc/`, `~/.ssh/` 等) の warning: 誤検知が多く UX 悪化
//!   懸念。task #22 で UX 整備と合わせて検討
//! - path canonicalization の error message への露出: debug log 経路で逆に
//!   情報漏洩リスクがあるので、error message には元の path のみを残す

use std::io::Read;
use std::path::Path;

/// `file:` 経路で 1 度に読み込む最大 size の default。
///
/// DR-0006 §8.6 の「default 16MB」に従い 16 MiB を採用。CLI 段では
/// [`hyoui::cli::DEFAULT_INPUT_MAX_FILE_BYTES`] を使って解決済の値を
/// [`handle_file`] に渡すため、本定数は **test 専用**として残してある
/// (= 両者は同期している、handler 単体テストで実値を書きたくないため)。
#[cfg(test)]
pub const DEFAULT_MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// `text:<string>` を bytes 化。
///
/// DR-0006 §8.2 に従い **escape 解釈はしない** (= `\n` などの literal 化は
/// shell の責務、CLI 側で `--unescape` flag を入れる議論は別 task)。UTF-8
/// 文字列をそのまま bytes 列にする。
pub fn handle_text(value: &str) -> Vec<u8> {
    value.as_bytes().to_vec()
}

/// `hex:<hex>` を bytes 化。
///
/// parser 段 (= `cli::parse_hex_value`) で既に `Vec<u8>` に decode 済なので、
/// 本 handler は clone するだけ。一貫性のため handler 経路を通す。
pub fn handle_hex(bytes: &[u8]) -> Vec<u8> {
    bytes.to_vec()
}

/// `file:<path>` を読み込んで bytes 化。
///
/// `path == "-"` (= stdin) のときは stdin から bytes を読み切る。それ以外は
/// 通常 file として読み込む。
///
/// task #21 (= path validation セキュリティ視点) で以下の防御を追加:
///
/// - **size 上限**: `max_bytes` 引数を超える file は read 前に reject (= metadata
///   の size を先に見て、巨大 file を読み始めてから止める動きを回避)。`max_bytes
///   == 0` のときは無制限扱い。stdin 経路でも同じ上限を適用する
/// - **regular file 限定**: directory / device / socket / fifo は reject。
///   symlink は OS が follow した結果の metadata で判定するため、target が
///   regular file ならば accept (= 安全側、無限 read 可能な device は弾く)
/// - **空 file の warning**: stderr に warning を出して bytes は空のまま続行
///   (= UX 優先、abort しない)
///
/// stdin (= `-`) は file type check の対象外 (= pipe / tty / regular file 何でも
/// 来うる)。size 上限のみ適用。
///
/// # Errors
///
/// - path が存在しない / 読み取り権限がない → "file: metadata 取得失敗 ..."
/// - directory / device / socket / fifo → "file: '<path>' は regular file ではありません ..."
/// - size 上限超過 → "file: '<path>': size <N> exceeds limit <M> bytes"
/// - stdin 読み取り失敗 → "file: stdin 読み取り失敗: ..."
/// - stdin の入力 size 超過 → "file: stdin の入力 size が上限 <M> bytes を超えています"
pub fn handle_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>, String> {
    if path.as_os_str() == "-" {
        // stdin を完全に読み切る (= EOF まで)。size 上限を超えたら error。
        // max_bytes == 0 は「無制限」扱い (= take せず全部読む)。
        let mut buf = Vec::new();
        let mut handle = std::io::stdin().lock();
        if max_bytes == 0 {
            handle
                .read_to_end(&mut buf)
                .map_err(|e| format!("file: stdin 読み取り失敗: {e}"))?;
        } else {
            // take(N+1) で「N byte 読み切れたら確実に over」と判定できる。
            let mut limited = (&mut handle).take(max_bytes.saturating_add(1));
            limited
                .read_to_end(&mut buf)
                .map_err(|e| format!("file: stdin 読み取り失敗: {e}"))?;
            if buf.len() as u64 > max_bytes {
                return Err(format!(
                    "file: stdin の入力 size が上限 {max_bytes} bytes を超えています"
                ));
            }
        }
        if buf.is_empty() {
            eprintln!("hyoui: warning: file:- (stdin) is empty, nothing to send");
        }
        return Ok(buf);
    }

    // 通常 file。`metadata` は symlink を follow するので「symlink → regular file」
    // は accept、「symlink → directory」「symlink → device」は file_type で
    // reject される。先に metadata で type / size を見て、巨大 file を読み始めて
    // から止める動きを避ける。
    let meta = std::fs::metadata(path)
        .map_err(|e| format!("file: metadata 取得失敗 ({}): {e}", path.display()))?;

    // file type 検証 (= regular file のみ accept)。directory / block / char /
    // socket / fifo は read しても意味がない or 危険なので reject。
    // (= `/dev/zero` のような char device を読み始めると無限 loop で size 制限が
    // 効くまで CPU を焼く可能性がある)
    if !meta.is_file() {
        let kind = describe_file_type(&meta);
        return Err(format!(
            "file: '{}' は regular file ではありません ({kind})",
            path.display()
        ));
    }

    let size = meta.len();
    if max_bytes > 0 && size > max_bytes {
        return Err(format!(
            "file: '{}': size {size} exceeds limit {max_bytes} bytes",
            path.display()
        ));
    }

    let bytes =
        std::fs::read(path).map_err(|e| format!("file: read 失敗 ({}): {e}", path.display()))?;

    if bytes.is_empty() {
        eprintln!(
            "hyoui: warning: file '{}' is empty, nothing to send",
            path.display()
        );
    }

    Ok(bytes)
}

/// `fs::Metadata` から file type の人間が読める短い説明を返す。
///
/// `is_file()` で reject されたときの error message に含めて、ユーザが原因を
/// 把握できるようにする (= "not a regular file" だけだと debug しにくい)。
fn describe_file_type(meta: &std::fs::Metadata) -> &'static str {
    let ft = meta.file_type();
    if ft.is_dir() {
        "directory"
    } else if ft.is_symlink() {
        // metadata() は follow するので通常ここには来ないが、symlink_metadata
        // 経由で来る将来拡張のために残す
        "symlink"
    } else {
        // unix の char / block / socket / fifo を細分化したいが、std::fs::FileType
        // の public API には Unix 固有の判定 (= `FileTypeExt::is_block_device` 等)
        // が必要で、import が複雑になる。MVP は "special file" でまとめる。
        // (= 「regular file じゃない」が伝わればユーザは path を見直せる)
        "special file (device/socket/fifo etc.)"
    }
}

/// bracketed paste の開始 / 終了 sequence。DEC モード `?2004` の paste mode。
const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

/// `paste:<string>` を bracketed paste で wrap した bytes 列にする。
///
/// 注意: 中身に `\x1b[201~` (= bracketed paste の終端マーカー) が含まれていると
/// 子 terminal 側で paste の終端と区別不能になり、後続 bytes が text mode で
/// 解釈される (= 任意コマンド実行に化ける危険)。本 handler は escape できない
/// (= TUI 仕様上 nest 不能) ため、含む文字列は **reject** する。
///
/// # Errors
///
/// 中身に `\x1b[201~` が含まれていれば "paste: 中身に bracketed paste 終端 ..." を返す。
pub fn handle_paste(value: &str) -> Result<Vec<u8>, String> {
    if value
        .as_bytes()
        .windows(BRACKETED_PASTE_END.len())
        .any(|w| w == BRACKETED_PASTE_END)
    {
        return Err(
            "paste: 中身に bracketed paste 終端 sequence (ESC [ 201 ~) が含まれており \
             nest 不能なため送信を拒否しました"
                .to_string(),
        );
    }
    let mut out =
        Vec::with_capacity(BRACKETED_PASTE_START.len() + value.len() + BRACKETED_PASTE_END.len());
    out.extend_from_slice(BRACKETED_PASTE_START);
    out.extend_from_slice(value.as_bytes());
    out.extend_from_slice(BRACKETED_PASTE_END);
    Ok(out)
}

/// `key:<name>` を escape sequence / control byte に変換する。
///
/// 対応一覧 (= DR-0006 §8.4 の主要 alias から MVP scope を絞ったもの):
///
/// - 制御文字: `C-a` 〜 `C-z` (= `\x01..\x1A`)、`C-@` `C-[` `C-\\` `C-]` `C-^` `C-_` `C-?`
/// - Meta: `M-x` (= ESC + x、1 文字限定の最小実装)
/// - 名前付き: `Enter`/`Return`、`Tab`、`Esc`/`Escape`、`Backspace`/`BS`、`Delete`/`Del`、
///   `Space`、`Up`/`Down`/`Left`/`Right`、`Home`/`End`/`PageUp`/`PageDown`、`F1`〜`F12`
/// - alias: `Ctrl-X` / `^X` も `C-X` と同義 (= 大文字小文字無視)
///
/// **multi-modifier (= `C-S-A` 等) は MVP 後回し** (= DR-0006 §8.4)。本 handler は
/// 検知したら error を返す。
///
/// # Errors
///
/// - 不明な key 名 → "key: 未知のキー名 ..."
/// - multi-modifier → "key: 複合 modifier ... は MVP 範囲外"
pub fn handle_key(name: &str) -> Result<Vec<u8>, String> {
    if name.is_empty() {
        return Err("key: key name が空です".to_string());
    }

    // 1. modifier prefix を正規化 (= alias を `C-` / `M-` に揃える)。
    //    `Ctrl-`, `Ctrl+`, `^` → `C-`
    //    `Alt-`, `Alt+`, `Meta-`, `M-`, `M+` → `M-`
    let normalized = normalize_modifier_prefix(name);

    // 2. Multi-modifier (= `C-S-` / `C-M-` 等) を弾く。`C-` で始まったあと
    //    残りに更に `C-` / `M-` / `S-` が出るパターンを検出。
    if has_multi_modifier(&normalized) {
        return Err(format!(
            "key: 複合 modifier {name:?} は MVP 範囲外 (= DR-0006 §8.4)"
        ));
    }

    // 3. Ctrl 系。
    if let Some(rest) = normalized.strip_prefix("C-") {
        return ctrl_byte(rest)
            .map(|b| vec![b])
            .ok_or_else(|| format!("key: 未知の Ctrl key {name:?}"));
    }

    // 4. Meta 系 (= ESC + char、1 文字限定の MVP)。
    if let Some(rest) = normalized.strip_prefix("M-") {
        // `M-x` は ESC + x。1 char 限定。複数 char は将来拡張 (= `M-Enter` 等)、
        // 現状は alphanumeric の 1 char のみサポート。
        let mut chars = rest.chars();
        let first = chars
            .next()
            .ok_or_else(|| format!("key: M- の後ろが空です: {name:?}"))?;
        if chars.next().is_some() {
            return Err(format!(
                "key: M- の後ろは 1 文字のみサポート ({name:?}、複数 char / 名前 key は将来拡張)"
            ));
        }
        if !first.is_ascii() {
            return Err(format!("key: M- の後ろは ASCII 1 文字のみ ({name:?})"));
        }
        let mut out = vec![0x1b];
        out.extend_from_slice(first.encode_utf8(&mut [0u8; 4]).as_bytes());
        return Ok(out);
    }

    // 5. 名前付き key (= Enter / Tab / 矢印 / F1-F12 等)。case-insensitive で照合。
    if let Some(bytes) = named_key_bytes(&normalized) {
        return Ok(bytes.to_vec());
    }

    // task #22: edit distance 1 以下で類似 key 名を suggest。
    // 候補は基本名 (= ASCII alias) のみ。Unicode alias は提案しない (= ユーザに
    // ASCII 名で覚えてもらうほうが grep / completion で扱いやすい)。
    let base = format!(
        "key: 未知のキー名 {name:?} (= サポート: Enter/Tab/Esc/Backspace/Delete/Space/\
         Up/Down/Left/Right/Home/End/PageUp/PageDown/F1..F12/C-<char>/M-<char>)"
    );
    if let Some(suggested) = suggest_key_name(name) {
        return Err(format!("{base} (did you mean `{suggested}`?)"));
    }
    Err(base)
}

/// `handle_key` 用の typo suggester (= ASCII case-insensitive Levenshtein 距離 1)。
///
/// `cli::suggest_closest` と同じ方針だが、key 名候補が key handler 固有なので
/// ここに専用 helper を置く (= cli の `INPUT_SPEC_PREFIXES` と並列の構造)。
fn suggest_key_name(name: &str) -> Option<&'static str> {
    /// `handle_key` がサポートする ASCII alias 一覧 (= suggest 用、Unicode は含めない)。
    const KEY_NAME_CANDIDATES: &[&str] = &[
        "Enter",
        "Return",
        "Tab",
        "Esc",
        "Escape",
        "Backspace",
        "Delete",
        "Space",
        "Up",
        "Down",
        "Left",
        "Right",
        "Home",
        "End",
        "PageUp",
        "PageDown",
        "F1",
        "F2",
        "F3",
        "F4",
        "F5",
        "F6",
        "F7",
        "F8",
        "F9",
        "F10",
        "F11",
        "F12",
    ];
    let input_lower = name.to_ascii_lowercase();
    let mut best: Option<(&'static str, usize)> = None;
    for cand in KEY_NAME_CANDIDATES {
        let cand_lower = cand.to_ascii_lowercase();
        let d = ascii_ci_levenshtein(&input_lower, &cand_lower);
        if d == 0 || d > 1 {
            continue;
        }
        match best {
            None => best = Some((cand, d)),
            Some((_, b)) if d < b => best = Some((cand, d)),
            _ => {}
        }
    }
    best.map(|(c, _)| c)
}

/// 1 次元 DP の Levenshtein 距離 (= ASCII 専用、cli::levenshtein_ascii_ci と同じ実装)。
fn ascii_ci_levenshtein(a: &str, b: &str) -> usize {
    let a = a.as_bytes();
    let b = b.as_bytes();
    let (m, n) = (a.len(), b.len());
    if m.abs_diff(n) > 2 {
        return usize::MAX;
    }
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

/// modifier prefix を `C-` / `M-` に正規化する。
///
/// - `Ctrl-X`, `Ctrl+X`, `ctrl-X`, `ctrl+X`, `^X` → `C-X`
/// - `Alt-X`, `Alt+X`, `alt-X`, `Meta-X`, `meta-X`, `M+X` → `M-X`
/// - 既に `C-`/`M-` で始まる場合はそのまま (= ただし `c-`/`m-` も大文字に直す)
/// - それ以外はそのまま return
fn normalize_modifier_prefix(name: &str) -> String {
    // case-insensitive prefix match を簡潔にするため lower で比較する。
    let lower = name.to_ascii_lowercase();
    if let Some(rest) = lower
        .strip_prefix("ctrl-")
        .or_else(|| lower.strip_prefix("ctrl+"))
        .or_else(|| lower.strip_prefix("c-"))
        .or_else(|| lower.strip_prefix("c+"))
    {
        // 元の `name` から prefix と同じ長さを切り取る (= rest の中身を元 case で残す)
        let prefix_len = name.len() - rest.len();
        return format!("C-{}", &name[prefix_len..]);
    }
    if let Some(rest) = name.strip_prefix('^') {
        return format!("C-{rest}");
    }
    // task #22: Unicode 修飾 alias (DR-0006 §8.4)。
    // `⌃` = Ctrl、`⌥`/`⎇` = Alt/Option/Meta。modifier の直後に区切り `-`/`+` を
    // 任意で許す (= `⌃-X` / `⌃X` / `⌃+X` を全て `C-X` に正規化)。
    // `⌘` (Command)、`⇧` (Shift)、`❖`/`Win` (Super) は MVP 未対応 — そのまま素通し
    // して下流の named_key_bytes / ctrl_byte で reject させる。
    if let Some(rest) = name.strip_prefix('⌃') {
        let rest = rest
            .strip_prefix('-')
            .or_else(|| rest.strip_prefix('+'))
            .unwrap_or(rest);
        return format!("C-{rest}");
    }
    if let Some(rest) = name.strip_prefix('⌥').or_else(|| name.strip_prefix('⎇')) {
        let rest = rest
            .strip_prefix('-')
            .or_else(|| rest.strip_prefix('+'))
            .unwrap_or(rest);
        return format!("M-{rest}");
    }
    if let Some(rest) = lower
        .strip_prefix("alt-")
        .or_else(|| lower.strip_prefix("alt+"))
        .or_else(|| lower.strip_prefix("meta-"))
        .or_else(|| lower.strip_prefix("meta+"))
        .or_else(|| lower.strip_prefix("m-"))
        .or_else(|| lower.strip_prefix("m+"))
    {
        let prefix_len = name.len() - rest.len();
        return format!("M-{}", &name[prefix_len..]);
    }
    name.to_string()
}

/// 正規化済 modifier prefix を含む文字列に追加 modifier (= 2 個目以降) があるか検査。
///
/// `C-S-A` は `C-` + `S-A` (= Shift modifier 残る) で multi。`C-a` は OK (= rest=`a`)。
fn has_multi_modifier(normalized: &str) -> bool {
    let rest = match normalized
        .strip_prefix("C-")
        .or_else(|| normalized.strip_prefix("M-"))
    {
        Some(r) => r,
        None => return false,
    };
    let rest_lower = rest.to_ascii_lowercase();
    rest_lower.starts_with("c-")
        || rest_lower.starts_with("m-")
        || rest_lower.starts_with("s-")
        || rest_lower.starts_with("shift-")
        || rest_lower.starts_with("ctrl-")
        || rest_lower.starts_with("alt-")
        || rest_lower.starts_with("meta-")
        || rest_lower.starts_with("super-")
        || rest_lower.starts_with("cmd-")
}

/// `C-<rest>` の rest を byte に変換 (= 古典的な伝統)。
///
/// `C-a` 〜 `C-z` (case-insensitive) は `\x01` 〜 `\x1A`。
/// 特殊文字も含む (DR-0006 §8.4 のテーブル)。
fn ctrl_byte(rest: &str) -> Option<u8> {
    // single char で処理する。本実装は ASCII letter / 特殊記号のみ対応。
    if rest.chars().count() != 1 {
        return None;
    }
    let c = rest.chars().next()?;
    match c {
        'a'..='z' => Some((c as u8) - b'a' + 1),
        'A'..='Z' => Some((c as u8) - b'A' + 1),
        '@' => Some(0x00),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        '?' => Some(0x7f),
        ' ' => Some(0x00), // `C-Space` = `C-@` = NUL (慣例)
        _ => None,
    }
}

/// 名前付き key (= 修飾なし) を escape sequence に変換。case-insensitive。
///
/// task #22 で DR-0006 §8.4 の Unicode alias (= `↩` / `⏎` / `⎋` 等) を追加した。
/// Unicode は to_ascii_lowercase で変化しないので、ASCII alias と同じ match arm に
/// そのまま列挙できる (= match は code-point 単位、大小は ASCII 範囲のみ判定)。
fn named_key_bytes(name: &str) -> Option<&'static [u8]> {
    match name.to_ascii_lowercase().as_str() {
        // 単一バイト (= Unicode alias 含む、DR-0006 §8.4)
        "enter" | "return" | "ret" | "↩" | "⏎" => Some(b"\r"),
        "tab" | "⇥" => Some(b"\t"),
        "esc" | "escape" | "⎋" => Some(b"\x1b"),
        "backspace" | "bs" | "⌫" => Some(b"\x7f"),
        "delete" | "del" | "⌦" => Some(b"\x1b[3~"),
        "space" | "sp" | "␣" => Some(b" "),
        // 矢印 (= CSI sequence)。Unicode 矢印 alias を同居。
        "up" | "↑" => Some(b"\x1b[A"),
        "down" | "↓" => Some(b"\x1b[B"),
        "right" | "→" => Some(b"\x1b[C"),
        "left" | "←" => Some(b"\x1b[D"),
        // navigation
        "home" | "⤒" => Some(b"\x1b[H"),
        "end" | "⤓" => Some(b"\x1b[F"),
        "pageup" | "pgup" | "page-up" | "⇞" => Some(b"\x1b[5~"),
        "pagedown" | "pgdn" | "page-down" | "⇟" => Some(b"\x1b[6~"),
        // Function keys: F1-F4 は SS3 (= ESC O P/Q/R/S)、F5-F12 は CSI ~ 形式
        "f1" => Some(b"\x1bOP"),
        "f2" => Some(b"\x1bOQ"),
        "f3" => Some(b"\x1bOR"),
        "f4" => Some(b"\x1bOS"),
        "f5" => Some(b"\x1b[15~"),
        "f6" => Some(b"\x1b[17~"),
        "f7" => Some(b"\x1b[18~"),
        "f8" => Some(b"\x1b[19~"),
        "f9" => Some(b"\x1b[20~"),
        "f10" => Some(b"\x1b[21~"),
        "f11" => Some(b"\x1b[23~"),
        "f12" => Some(b"\x1b[24~"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- text ---
    #[test]
    fn text_returns_bytes_as_is() {
        assert_eq!(handle_text("hello"), b"hello");
        assert_eq!(handle_text(""), b"");
        // escape は CLI 側でやらない (= shell 任せ)
        assert_eq!(handle_text("a\\nb"), b"a\\nb");
        // UTF-8
        assert_eq!(handle_text("あ"), "あ".as_bytes());
    }

    // --- hex ---
    #[test]
    fn hex_returns_bytes_clone() {
        assert_eq!(handle_hex(&[0x1b, 0x5b, 0x41]), vec![0x1b, 0x5b, 0x41]);
        assert_eq!(handle_hex(&[]), Vec::<u8>::new());
    }

    // --- paste ---
    #[test]
    fn paste_wraps_with_bracketed_paste() {
        let got = handle_paste("hello").expect("wrap");
        assert_eq!(got, b"\x1b[200~hello\x1b[201~");
    }

    #[test]
    fn paste_empty_string_still_wraps() {
        let got = handle_paste("").expect("wrap");
        assert_eq!(got, b"\x1b[200~\x1b[201~");
    }

    #[test]
    fn paste_rejects_embedded_end_marker() {
        let err = handle_paste("safe\x1b[201~malicious").unwrap_err();
        assert!(
            err.contains("bracketed paste 終端"),
            "expected reject message, got: {err}"
        );
    }

    /// QA edge: paste 中身に bracketed paste **開始** マーカー (= `ESC[200~`)
    /// が含まれるのは reject **しない**。終端 (= `ESC[201~`) のみが nest 不能
    /// として reject 対象 (= 子側で `ESC[200~` が来ても無害、現在 paste mode の
    /// 上書きにしかならない)。仕様確認のための保護 test。
    #[test]
    fn paste_accepts_embedded_start_marker() {
        let got = handle_paste("before\x1b[200~after").expect("accept");
        // 開始マーカーは中身として透過、外側で再度 ESC[200~/ESC[201~ で wrap される
        assert_eq!(&got[..6], b"\x1b[200~");
        assert!(got.ends_with(b"\x1b[201~"));
        assert!(got.windows(6).any(|w| w == b"\x1b[200~"));
    }

    /// QA edge: paste 中身に改行が含まれる (= multi-line) 場合、bytes そのまま
    /// 透過される。`--line-ending=preserve` (default) の動作確認。
    #[test]
    fn paste_preserves_newlines() {
        let got = handle_paste("line1\nline2\nline3").expect("wrap");
        assert_eq!(got, b"\x1b[200~line1\nline2\nline3\x1b[201~");
    }

    // --- key: ctrl ---
    #[test]
    fn key_ctrl_lowercase_letters() {
        assert_eq!(handle_key("C-a").unwrap(), vec![0x01]);
        assert_eq!(handle_key("C-c").unwrap(), vec![0x03]);
        assert_eq!(handle_key("C-z").unwrap(), vec![0x1A]);
    }

    #[test]
    fn key_ctrl_case_insensitive() {
        // C-A も C-a と同じ (= modifier ありは case-insensitive、DR-0006 §8.4)
        assert_eq!(handle_key("C-A").unwrap(), vec![0x01]);
    }

    #[test]
    fn key_ctrl_aliases() {
        // Ctrl-A / Ctrl+A / ^A / ctrl-a すべて C-a と同義
        assert_eq!(handle_key("Ctrl-A").unwrap(), vec![0x01]);
        assert_eq!(handle_key("Ctrl+A").unwrap(), vec![0x01]);
        assert_eq!(handle_key("^A").unwrap(), vec![0x01]);
        assert_eq!(handle_key("ctrl-a").unwrap(), vec![0x01]);
    }

    #[test]
    fn key_ctrl_specials() {
        assert_eq!(handle_key("C-[").unwrap(), vec![0x1b]);
        assert_eq!(handle_key("C-]").unwrap(), vec![0x1d]);
        assert_eq!(handle_key("C-\\").unwrap(), vec![0x1c]);
        assert_eq!(handle_key("C-^").unwrap(), vec![0x1e]);
        assert_eq!(handle_key("C-_").unwrap(), vec![0x1f]);
        assert_eq!(handle_key("C-?").unwrap(), vec![0x7f]);
        assert_eq!(handle_key("C-@").unwrap(), vec![0x00]);
    }

    // --- key: meta ---
    #[test]
    fn key_meta_ascii_char() {
        assert_eq!(handle_key("M-x").unwrap(), vec![0x1b, b'x']);
        assert_eq!(handle_key("Alt-a").unwrap(), vec![0x1b, b'a']);
        assert_eq!(handle_key("Meta-Z").unwrap(), vec![0x1b, b'Z']);
    }

    #[test]
    fn key_meta_multi_char_rejected() {
        let err = handle_key("M-Enter").unwrap_err();
        assert!(err.contains("1 文字のみ"), "got: {err}");
    }

    // --- key: named ---
    #[test]
    fn key_named_basic() {
        assert_eq!(handle_key("Enter").unwrap(), b"\r");
        assert_eq!(handle_key("Tab").unwrap(), b"\t");
        assert_eq!(handle_key("Esc").unwrap(), b"\x1b");
        assert_eq!(handle_key("Backspace").unwrap(), b"\x7f");
        assert_eq!(handle_key("Delete").unwrap(), b"\x1b[3~");
        assert_eq!(handle_key("Space").unwrap(), b" ");
    }

    #[test]
    fn key_named_arrows() {
        assert_eq!(handle_key("Up").unwrap(), b"\x1b[A");
        assert_eq!(handle_key("Down").unwrap(), b"\x1b[B");
        assert_eq!(handle_key("Right").unwrap(), b"\x1b[C");
        assert_eq!(handle_key("Left").unwrap(), b"\x1b[D");
    }

    #[test]
    fn key_named_navigation() {
        assert_eq!(handle_key("Home").unwrap(), b"\x1b[H");
        assert_eq!(handle_key("End").unwrap(), b"\x1b[F");
        assert_eq!(handle_key("PageUp").unwrap(), b"\x1b[5~");
        assert_eq!(handle_key("PageDown").unwrap(), b"\x1b[6~");
    }

    #[test]
    fn key_named_function_keys() {
        assert_eq!(handle_key("F1").unwrap(), b"\x1bOP");
        assert_eq!(handle_key("F4").unwrap(), b"\x1bOS");
        assert_eq!(handle_key("F5").unwrap(), b"\x1b[15~");
        assert_eq!(handle_key("F12").unwrap(), b"\x1b[24~");
    }

    #[test]
    fn key_named_case_insensitive() {
        assert_eq!(handle_key("enter").unwrap(), b"\r");
        assert_eq!(handle_key("ENTER").unwrap(), b"\r");
        assert_eq!(handle_key("EnTeR").unwrap(), b"\r");
    }

    #[test]
    fn key_unknown_name_rejected() {
        let err = handle_key("Foobar").unwrap_err();
        assert!(err.contains("未知のキー名"), "got: {err}");
    }

    #[test]
    fn key_multi_modifier_rejected() {
        // C-S-a (Ctrl-Shift-A) は MVP 範囲外
        let err = handle_key("C-S-a").unwrap_err();
        assert!(err.contains("複合 modifier"), "got: {err}");
        // C-M-x (Ctrl-Alt-X)
        let err = handle_key("C-M-x").unwrap_err();
        assert!(err.contains("複合 modifier"), "got: {err}");
    }

    /// QA edge: modifier だけで key 部が空 (= `C-`、`M-`) は意味のない入力。
    /// 暗黙の panic ではなく error として返ることを保護する (= `ctrl_byte("")` /
    /// `M-` 経路の防御確認)。
    #[test]
    fn key_modifier_without_keyname_rejected() {
        let err = handle_key("C-").unwrap_err();
        assert!(
            err.contains("Ctrl key") || err.contains("未知"),
            "got: {err}"
        );
        let err = handle_key("M-").unwrap_err();
        assert!(err.contains("M-") || err.contains("空"), "got: {err}");
    }

    /// QA edge: trailing whitespace を含む key 名 (= `"Enter "`) は trim せず
    /// 未知扱いとして error (= 規範: ユーザ責任で正規化させる、暗黙 trim しない)。
    #[test]
    fn key_trailing_whitespace_rejected() {
        // 末尾 space つき → named_key_bytes が見つけられず未知扱い
        let err = handle_key("Enter ").unwrap_err();
        assert!(err.contains("未知のキー名"), "got: {err}");
    }

    #[test]
    fn key_empty_rejected() {
        let err = handle_key("").unwrap_err();
        assert!(err.contains("空"), "got: {err}");
    }

    // --- task #22: Unicode key alias (DR-0006 §8.4) ---

    #[test]
    fn key_unicode_alias_enter() {
        assert_eq!(handle_key("↩").unwrap(), b"\r");
        assert_eq!(handle_key("⏎").unwrap(), b"\r");
    }

    #[test]
    fn key_unicode_alias_esc_backspace_delete() {
        assert_eq!(handle_key("⎋").unwrap(), b"\x1b");
        assert_eq!(handle_key("⌫").unwrap(), b"\x7f");
        assert_eq!(handle_key("⌦").unwrap(), b"\x1b[3~");
    }

    #[test]
    fn key_unicode_alias_tab_and_space() {
        assert_eq!(handle_key("⇥").unwrap(), b"\t");
        assert_eq!(handle_key("␣").unwrap(), b" ");
    }

    #[test]
    fn key_unicode_alias_arrows() {
        assert_eq!(handle_key("↑").unwrap(), b"\x1b[A");
        assert_eq!(handle_key("↓").unwrap(), b"\x1b[B");
        assert_eq!(handle_key("→").unwrap(), b"\x1b[C");
        assert_eq!(handle_key("←").unwrap(), b"\x1b[D");
    }

    #[test]
    fn key_unicode_alias_navigation() {
        assert_eq!(handle_key("⤒").unwrap(), b"\x1b[H");
        assert_eq!(handle_key("⤓").unwrap(), b"\x1b[F");
        assert_eq!(handle_key("⇞").unwrap(), b"\x1b[5~");
        assert_eq!(handle_key("⇟").unwrap(), b"\x1b[6~");
    }

    #[test]
    fn key_unicode_ctrl_modifier() {
        // `⌃X` = `C-X` = SOH (= 0x18)
        assert_eq!(handle_key("⌃X").unwrap(), vec![0x18]);
        // separator あり / なし両対応
        assert_eq!(handle_key("⌃-a").unwrap(), vec![0x01]);
        assert_eq!(handle_key("⌃+a").unwrap(), vec![0x01]);
    }

    #[test]
    fn key_unicode_alt_modifier() {
        // `⌥x` = `M-x` = ESC + 'x'
        assert_eq!(handle_key("⌥x").unwrap(), vec![0x1b, b'x']);
        // `⎇x` も同じく Alt として正規化
        assert_eq!(handle_key("⎇x").unwrap(), vec![0x1b, b'x']);
    }

    #[test]
    fn key_unicode_unsupported_modifiers_rejected() {
        // `⌘` (Command) は MVP 未対応 → そのまま素通しして named/ctrl で reject。
        let err = handle_key("⌘x").unwrap_err();
        assert!(err.contains("未知のキー名"), "got: {err}");
    }

    // --- task #22: key 名 typo suggest ---

    #[test]
    fn key_typo_suggests_enter() {
        let err = handle_key("Entr").unwrap_err();
        assert!(err.contains("did you mean `Enter`"), "got: {err}");
    }

    #[test]
    fn key_typo_suggests_backspace() {
        let err = handle_key("Backspce").unwrap_err();
        assert!(err.contains("did you mean `Backspace`"), "got: {err}");
    }

    #[test]
    fn key_far_typo_no_suggest() {
        // 距離 2 以上は suggest しない (= 誤候補を増やさない方針)
        let err = handle_key("xyzzy").unwrap_err();
        assert!(!err.contains("did you mean"), "got: {err}");
    }

    // --- file ---
    #[test]
    fn file_reads_regular_file() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("payload.bin");
        let content = b"hello\nworld\n";
        std::fs::write(&path, content).expect("write");
        let got = handle_file(&path, DEFAULT_MAX_FILE_BYTES).expect("read");
        assert_eq!(got, content);
    }

    #[test]
    fn file_missing_returns_error() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("nonexistent");
        let err = handle_file(&path, DEFAULT_MAX_FILE_BYTES).unwrap_err();
        assert!(
            err.contains("metadata 取得失敗") || err.contains("read 失敗"),
            "got: {err}"
        );
    }

    #[test]
    fn file_oversized_rejected() {
        // 16 MiB + 1 byte 相当の sparse file を作る (= seek + write 1 byte で実 disk
        // 容量は食わない)。Rust だと `set_len` で OK。
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("big");
        let f = std::fs::File::create(&path).expect("create");
        f.set_len(DEFAULT_MAX_FILE_BYTES + 1).expect("set_len");
        drop(f);
        let err = handle_file(&path, DEFAULT_MAX_FILE_BYTES).unwrap_err();
        assert!(err.contains("exceeds limit"), "got: {err}");
    }

    /// task #21: `--max-file-bytes` で override されたときに、default より小さい
    /// 上限でも reject されることを確認。CLI flag 経路の動作保証。
    #[test]
    fn file_custom_max_bytes_rejected() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("small");
        std::fs::write(&path, b"hello world\n").expect("write"); // 12 bytes
        // 上限 8 bytes だと「12 > 8」で reject。
        let err = handle_file(&path, 8).unwrap_err();
        assert!(err.contains("exceeds limit 8"), "got: {err}");
    }

    /// task #21: `max_bytes == 0` は無制限扱い (= size check を skip)。
    #[test]
    fn file_zero_max_bytes_unlimited() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("small");
        std::fs::write(&path, b"any content").expect("write");
        let got = handle_file(&path, 0).expect("read");
        assert_eq!(got, b"any content");
    }

    /// task #21: directory を file: で指定したら error。
    #[test]
    fn file_directory_rejected() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let err = handle_file(dir.path(), DEFAULT_MAX_FILE_BYTES).unwrap_err();
        assert!(err.contains("regular file ではありません"), "got: {err}");
        assert!(err.contains("directory"), "got: {err}");
    }

    /// task #21: symlink to regular file は accept (= follow した先が regular なら OK)。
    #[cfg(unix)]
    #[test]
    fn file_symlink_to_regular_file_accepted() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let target = dir.path().join("target.txt");
        std::fs::write(&target, b"via symlink").expect("write");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let got = handle_file(&link, DEFAULT_MAX_FILE_BYTES).expect("read");
        assert_eq!(got, b"via symlink");
    }

    /// task #21: symlink to directory は reject (= follow した先が directory)。
    #[cfg(unix)]
    #[test]
    fn file_symlink_to_directory_rejected() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let target_dir = dir.path().join("subdir");
        std::fs::create_dir(&target_dir).expect("mkdir");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target_dir, &link).expect("symlink");
        let err = handle_file(&link, DEFAULT_MAX_FILE_BYTES).unwrap_err();
        assert!(err.contains("regular file ではありません"), "got: {err}");
    }

    /// task #21: device file (= /dev/null) は reject。
    /// /dev/null は char device で、socket でも fifo でもないが metadata.is_file()
    /// が false になる代表例。CI / 開発環境で確実に存在する。
    #[cfg(unix)]
    #[test]
    fn file_device_file_rejected() {
        let path = Path::new("/dev/null");
        // /dev/null が読めない環境では skip (= 一部の sandbox 等)
        if std::fs::metadata(path).is_err() {
            return;
        }
        let err = handle_file(path, DEFAULT_MAX_FILE_BYTES).unwrap_err();
        assert!(err.contains("regular file ではありません"), "got: {err}");
    }

    /// task #21: 空 file は warning を出して bytes 空で続行する (= abort しない)。
    /// warning 文言の検証は stderr capture が必要だが、本 test では「error にならず
    /// 空 Vec が返る」ことだけを確認する (= warning は副作用、handler の戻り値で
    /// 表現しない方針)。
    #[test]
    fn file_empty_returns_empty_bytes_without_error() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("empty");
        std::fs::write(&path, b"").expect("write");
        let got = handle_file(&path, DEFAULT_MAX_FILE_BYTES).expect("read");
        assert!(
            got.is_empty(),
            "expected empty bytes, got {} bytes",
            got.len()
        );
    }
}
