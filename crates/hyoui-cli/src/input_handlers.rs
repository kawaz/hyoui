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
//! - `file:` の path validation (= task #21) も範囲外、bare な `std::fs::read`

use std::io::Read;
use std::path::Path;

/// `file:` 経路で 1 度に読み込む最大 size。
///
/// DR-0006 §8.6 で「default 16MB、0 で無制限」とされているが、本 task では
/// override flag を露出しない最小実装。`--max-file-bytes` 等の CLI 引数を
/// 受ける形は別 task (= #21 path validation と同じ tranche) で配線する。
pub(crate) const DEFAULT_MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// `text:<string>` を bytes 化。
///
/// DR-0006 §8.2 に従い **escape 解釈はしない** (= `\n` などの literal 化は
/// shell の責務、CLI 側で `--unescape` flag を入れる議論は別 task)。UTF-8
/// 文字列をそのまま bytes 列にする。
pub(crate) fn handle_text(value: &str) -> Vec<u8> {
    value.as_bytes().to_vec()
}

/// `hex:<hex>` を bytes 化。
///
/// parser 段 (= `cli::parse_hex_value`) で既に `Vec<u8>` に decode 済なので、
/// 本 handler は clone するだけ。一貫性のため handler 経路を通す。
pub(crate) fn handle_hex(bytes: &[u8]) -> Vec<u8> {
    bytes.to_vec()
}

/// `file:<path>` を読み込んで bytes 化。
///
/// `path == "-"` (= stdin) のときは stdin から bytes を読み切る。それ以外は
/// 通常 file として読み込む。size 上限 `DEFAULT_MAX_FILE_BYTES` を超える場合は
/// error (= atomic 保証、1 byte も送らずに失敗)。
///
/// **path validation は task #21 の scope**。本 handler は bare な
/// `std::fs::read` 同等 (= symlink follow、相対 path 許容)。
///
/// # Errors
///
/// - path が存在しない / 読み取り権限がない → "file: read 失敗: ..."
/// - size 上限超過 → "file: size 上限 ... を超えています ..."
/// - stdin 読み取り失敗 → "file: stdin 読み取り失敗: ..."
pub(crate) fn handle_file(path: &Path) -> Result<Vec<u8>, String> {
    if path.as_os_str() == "-" {
        // stdin を完全に読み切る (= EOF まで)。size 上限を超えたら error。
        let mut buf = Vec::new();
        let mut handle = std::io::stdin().lock();
        // take(N+1) で「N byte 読み切れたら確実に over」と判定できる。
        let max = DEFAULT_MAX_FILE_BYTES;
        let mut limited = (&mut handle).take(max.saturating_add(1));
        limited
            .read_to_end(&mut buf)
            .map_err(|e| format!("file: stdin 読み取り失敗: {e}"))?;
        if buf.len() as u64 > max {
            return Err(format!(
                "file: stdin の入力 size が上限 {max} bytes を超えています"
            ));
        }
        return Ok(buf);
    }

    // 通常 file。先に metadata で size を見て上限を超えていれば read せず error
    // (= 大きい file を全部読んでから捨てる動きを避ける)。symlink 先の size を
    // 見るため `metadata` (= follow) を使う。`std::fs::read` も follow なので
    // 挙動を揃える。
    let meta = std::fs::metadata(path)
        .map_err(|e| format!("file: metadata 取得失敗 ({}): {e}", path.display()))?;
    let size = meta.len();
    if size > DEFAULT_MAX_FILE_BYTES {
        return Err(format!(
            "file: size 上限 {DEFAULT_MAX_FILE_BYTES} bytes を超えています (file size: {size}, path: {})",
            path.display()
        ));
    }
    std::fs::read(path).map_err(|e| format!("file: read 失敗 ({}): {e}", path.display()))
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
pub(crate) fn handle_paste(value: &str) -> Result<Vec<u8>, String> {
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
pub(crate) fn handle_key(name: &str) -> Result<Vec<u8>, String> {
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

    Err(format!(
        "key: 未知のキー名 {name:?} (= サポート: Enter/Tab/Esc/Backspace/Delete/Space/\
         Up/Down/Left/Right/Home/End/PageUp/PageDown/F1..F12/C-<char>/M-<char>)"
    ))
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
fn named_key_bytes(name: &str) -> Option<&'static [u8]> {
    match name.to_ascii_lowercase().as_str() {
        // 単一バイト
        "enter" | "return" | "ret" => Some(b"\r"),
        "tab" => Some(b"\t"),
        "esc" | "escape" => Some(b"\x1b"),
        "backspace" | "bs" => Some(b"\x7f"),
        "delete" | "del" => Some(b"\x1b[3~"),
        "space" | "sp" => Some(b" "),
        // 矢印 (= CSI sequence)
        "up" => Some(b"\x1b[A"),
        "down" => Some(b"\x1b[B"),
        "right" => Some(b"\x1b[C"),
        "left" => Some(b"\x1b[D"),
        // navigation
        "home" => Some(b"\x1b[H"),
        "end" => Some(b"\x1b[F"),
        "pageup" | "pgup" | "page-up" => Some(b"\x1b[5~"),
        "pagedown" | "pgdn" | "page-down" => Some(b"\x1b[6~"),
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

    #[test]
    fn key_empty_rejected() {
        let err = handle_key("").unwrap_err();
        assert!(err.contains("空"), "got: {err}");
    }

    // --- file ---
    #[test]
    fn file_reads_regular_file() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("payload.bin");
        let content = b"hello\nworld\n";
        std::fs::write(&path, content).expect("write");
        let got = handle_file(&path).expect("read");
        assert_eq!(got, content);
    }

    #[test]
    fn file_missing_returns_error() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("nonexistent");
        let err = handle_file(&path).unwrap_err();
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
        let err = handle_file(&path).unwrap_err();
        assert!(err.contains("size 上限"), "got: {err}");
    }
}
