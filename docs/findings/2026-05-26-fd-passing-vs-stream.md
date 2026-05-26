# Finding: SCM_RIGHTS fd passing は動くが本実装では不採用

- Date: 2026-05-26
- PoC: `crates/hyoui/examples/04-fd-passing.rs`
- 関連: [[DR-0006]] §1 (Architecture), [[DR-0007]] v0.2.0 (HTTP gateway)

## 判明した事実

1. **SCM_RIGHTS + sendmsg/recvmsg で fd を別プロセスに渡せる**: macOS Darwin で動作確認、PASS
2. **渡された fd は kernel が dup したもの**: parent fd 4 を送ったが child では fd 3 として受け取る (kernel が空き fd を割り当て)、同じ file を指す (= write すると同じ inode に反映)
3. **nix crate の `recvmsg`/`sendmsg` は `uio` feature 必須**: 現 hyoui workspace では nix の features に `uio` が無いので、libc 直接で書く必要があった (or features 追加)
4. **libc 直接実装でも 60 行程度**: msghdr の zeroed init + iovec + CMSG_FIRSTHDR/CMSG_DATA マクロ操作で完結

## 実用的な示唆

### hyoui 本実装で SCM_RIGHTS を採用するか?

**しない**。理由:

| 観点 | stream 中継 | SCM_RIGHTS fd passing |
|---|---|---|
| 実装複雑度 | 低 (bytes コピー 1 回) | 高 (msghdr/cmsg 操作 + fd lifecycle 管理) |
| transport 抽象化 | ⭕ 同じ wire format で Unix socket / TCP / WebSocket | ❌ Unix socket 専用 |
| HTTP gateway (v0.2.0) 対応 | ⭕ そのまま動く | ❌ WebSocket で fd 渡せない |
| 性能差 | 1 hop (= 1 user-space copy) | 0 hop (= 直接 client fd に write) |
| client detach 時の fd 引き戻し | 不要 (= stream close で済む) | 必要 (close + 再 attach で再 passing) |

性能差は **microbenchmark でしか見えない** レベル (= 1 user-space copy は数百 ns 〜 数 μs、TUI app の更新頻度 (10-100Hz) では誤差)。複雑度・抽象化の犠牲に見合わない。

### 結論: stream 中継採用 ([[DR-0006]] §1 と整合)

- daemon は子 pty の出力を bytes として client socket に write (= 全 client に broadcast)
- daemon は client socket から bytes を read → 子 pty master に write (= multiplex)
- protocol message は length-prefixed bytes、transport は Unix socket (MVP) → TCP/WebSocket (v0.2.0) で同形

### SCM_RIGHTS の存在意義 (将来の参考)

- Unix domain のみで完結する高性能 IPC が欲しくなったら、ここに戻る選択肢はある
- 例: 巨大ファイルの zero-copy 転送、特殊権限の fd 移譲 (= bind 済 socket を他プロセスに渡す等)
- 本 PoC コードは参考実装として残す価値あり

## hyoui 本実装への反映

### protocol 設計の方向 (DR-0008 で詰める想定)

```
message kinds (transport-independent):
  HANDSHAKE { capabilities, mode: rw | ro | leader-no, lock_token }
  DATA_FROM_CHILD { bytes }       # daemon → client (broadcast)
  DATA_FROM_CLIENT { bytes }      # client → daemon (子 pty へ multiplex)
  RESIZE { cols, rows }           # client → daemon (leader のみ反映)
  SIGNAL { sig }                  # client → daemon (= 子に転送、現在は data の制御文字で代替)
  LOCK_REQUEST { ... }
  LOCK_RELEASE { token }
  LEADER_CHANGE { ... }
  STATUS_QUERY / STATUS_REPLY
  TAIL_REQUEST { since, last, follow, strip }
  WAIT_REQUEST { ... }
  ...
```

wire format は length-prefixed binary (= 既存 `protocol.rs` を拡張)。serde + bincode or msgpack で encode、transport は意識しない。

### Client lifecycle

```
1. client が socket connect
2. HANDSHAKE 交換 (capabilities + mode)
3. daemon が client_id 発行、broadcast list に追加
4. 以降 message を双方向で交換
5. client 切断 → daemon が detect (read EOF) → broadcast list から除外 + leader cascade
```

これで Unix socket / TCP / WebSocket すべて同じ flow で動く。

## 検証の詳細

```
$ cargo run --example 04-fd-passing
[parent] target file opened, fd=4
[parent] sent fd via SCM_RIGHTS, 8 data bytes
[child] recv data: "hello-fd"
[child] received fd 3
[child] wrote to received fd
[parent] target file content: "received via SCM_RIGHTS\n"
[parent] PASS
```

parent (fd 4) → child (fd 3) で kernel が dup、同じ target file に child が write → parent が読み取れる。SCM_RIGHTS の動作モデルとして正常。

### nix crate features の話

```toml
# hyoui workspace nix features (現状):
nix = { features = ["fs", "poll", "process", "signal", "socket", "net", "term",
                    "time", "event", "user", "ioctl"] }
# 不足: "uio" (sendmsg/recvmsg/iovec 系)
```

SCM_RIGHTS を本実装で使うなら `uio` feature 追加。**使わない判断なので追加不要**。

ただし将来 protocol で iovec ベースの scatter/gather I/O (= zero-copy 風味の send/recv) が欲しくなったら追加検討。現状は素朴な `read`/`write` で十分。
