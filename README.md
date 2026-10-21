# office-display

2Fオフィスの常設ディスプレイ（フォトフレーム）と、その周辺の小さな仕組みをまとめたリポジトリです。

実装は2つだけです。

1. **photo-frame-rs** — 写真をコラージュ状に表示するネイティブRustアプリ（egui/eframe + OpenGL/X11）。ブラウザもWebViewも使いません。
2. **presence-dimmer** — カメラの顔検出を見て、無人なら画面を暗くし、人が来たら戻す小さなデーモン。

このリポジトリには**写真そのもの・認証情報・社内ホスト名やIPアドレスは含めていません**。写真は表示機のローカルHTTPプロキシから取得し、このアプリは認証を行いません。

## 全体像

```
                    ┌────────────────────────── 表示機 (Linux, X11 :1) ──┐
                    │                                                    │
  Immich / 写真 ──▶ │  photo-manager (loopback :8081)                    │
                    │        ▲                                           │
                    │        │ GET /api/photos, /photos/<name>           │
                    │  photo-frame-rs ──────────────┐                    │
                    │   (fullscreen / OpenGL)       │ live camera tile    │
                    │                               ▼                    │
                    │  camera  (loopback :8082) ── MJPEG ──┐              │
                    │                                      │              │
                    │  face detector (loopback :8093) ◀────┘              │
                    │        ▲                                           │
                    │        │ GET /status (faces, healthy, age)         │
                    │  presence-dimmer ── xrandr --brightness ──▶ HDMI   │
                    └────────────────────────────────────────────────────┘
```

- 表示機の外に出る通信はしません（アプリ側はすべて `127.0.0.1`）。
- 顔検出は「何人いるか」だけを使い、身元特定・履歴保存はしません。
- カメラ映像はメモリ上だけで扱い、ディスクに残しません。

## photo-frame-rs

写真を2〜4枚のコラージュとして表示し、一定時間ごとに入れ替えます。

### 主なフラグ

| フラグ | 意味 |
|---|---|
| `--fullscreen` | 全画面表示 |
| `--scale N` | 論理ピクセル比（4Kなら `2`） |
| `--camera` / `--no-camera` | 右下のカメラプレビュー |
| `--live-tile` / `--no-live-tile` | カメラ映像をコラージュの1枚として描く（`--camera` を含意） |
| `--status-port 8096` | `GET /health` `/status`（ループバック限定） |
| `--run-seconds N` | 自動終了（動作確認用） |

`--live-tile` は、いちばん大きなカードを選び、カメラ映像をそのカードに収まるようにクロップして描きます。写真のレイアウト計算には手を入れず、描画だけを差し替えるので、レイアウトの不変条件を壊しません。

### ビルド

Rust 1.99 以降。

```sh
cd photo-frame-rs
cargo test --locked
cargo build --release --locked
# => target/release/photo-frame-rs
```

実行時はOpenGL/EGL/MesaとX11のライブラリが必要です（動的ロードのため `ldd` には出ません）。

### 状態の確認

```sh
curl -s http://127.0.0.1:8096/health
curl -s http://127.0.0.1:8096/status | jq .
```

`camera_state`・`camera_rendering_frames`・`content_generation` などが見えます。

## presence-dimmer

顔検出サービスの `/status` を2秒ごとに読み、次のように動きます。

- 一定時間（既定300秒）だれも検出されない → `xrandr --brightness 0.25` へフェード
- 顔が検出された → 1秒以内に全輝度へ復帰
- 検出器が落ちている／データが古い → **安全側**（全輝度のまま）

設定は systemd の `Environment=` で行います。

| 変数 | 既定 | 意味 |
|---|---|---|
| `PRESENCE_OUTPUT` | `HDMI-2` | 対象のRandR出力 |
| `PRESENCE_IDLE_SECONDS` | `300` | 暗くするまでの無検知時間 |
| `PRESENCE_DIM` | `0.25` | 暗くするときの輝度 |
| `PRESENCE_STATUS_URL` | `http://127.0.0.1:8093/status` | 顔検出の状態 |

## 配備

表示機側のユーザー名・ホスト名は環境変数で渡します（リポジトリには書きません）。

```sh
export DISPLAY_HOST=<display-host>
export DISPLAY_USER=<display-user>
export APP_ROOT=/home/$DISPLAY_USER/photo-frame-rs

./deploy/build-and-deploy.sh
```

スクリプトは次を行います。

1. `photo-frame-rs` を表示機へ `rsync`
2. 表示機上で `cargo build --release --locked`
3. `$APP_ROOT/releases/<version>-<date>/bin/` へ配置し、`current` を差し替え
4. systemdユニットを（初回のみ）設置して再起動
5. `/health` で確認

### ロールバック

```sh
# 直前のリリースへ戻す
ssh "$DISPLAY_USER@$DISPLAY_HOST" '
  cd ~/photo-frame-rs/releases && ls -1dt */ | sed -n 2p
'
```

`current` をそのディレクトリへ張り替えて `systemctl restart photo-frame-rs` します。

## 表示機のX起動

表示機は tty1 の自動ログインから `startx -- :1` でXを起動し、`.xinitrc` はXとウィンドウマネージャだけを立ち上げて `sleep infinity` で維持します。アプリ本体は systemd が管理します（`systemd/` 以下）。

- `DISPLAY=:1` を固定しているのは、unit側の `DISPLAY=:1` と一致させるためです。
- unit は `/tmp/.X11-unix/X1` ができるまで待ってから起動します（systemdのtarget順序とXの起動は無関係なので、競合を避けるため）。

## 方針

- **認証情報を置かない** — アプリはループバックのHTTPを読むだけで、トークンもAPIキーも持ちません。
- **写真を同梱しない** — 画像は実行時に取得します。
- **顔は数えるだけ** — 身元特定・保存はしません。
- **落ちても戻る** — systemd `Restart=always`、検出器異常時は明るいまま。
