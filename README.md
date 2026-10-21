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
                    │   (fullscreen / OpenGL)       │ live tile          │
                    │                               ▲                    │
                    │  room-survey (loopback :8094)│ 解析と描画         │
                    │        ▲            ▲         │                    │
                    │        │            │         │                    │
                    │  camera (8082)   mic (ALSA)   │                    │
                    │  face detector (loopback :8093) ─┘                 │
                    │        ▲                                           │
                    │        │ GET /status (faces, healthy, age)         │
                    │  presence-dimmer ── xrandr --brightness ──▶ HDMI   │
                    │                                                    │
                    │  office-stats (loopback :8095)                      │
                    │   1分ごとの集約をSQLiteへ / 週間グリッドを返す      │
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
| `--live-tile` / `--no-live-tile` | ライブ映像をコラージュの1枚として描く（`--camera` を含意） |
| `--camera-port N` | ライブ映像の取得先ポート（既定 8082、`room-survey` は 8094） |
| `--status-port 8096` | `GET /health` `/status`（ループバック限定） |
| `--run-seconds N` | 自動終了（動作確認用） |

`--live-tile` は、いちばん大きなカードを選び、そこにライブ映像を描きます。写真のレイアウト計算には手を入れず、描画だけを差し替えるので、レイアウトの不変条件を壊しません。

- カードに合う写真は描画をスキップし、カードの下地だけを残します
- 映像はアスペクトを保って収めます（contain）。切ると読み物として成立しないため
- 取得先は `--camera-port` で切り替えられます（既定は生カメラ 8082、計測図は 8094）

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

2秒ごとに2つの信号を見て、画面を「起こす／休ませる」だけの小さなデーモンです。

- **顔** — ローカルの顔検出 `/status` の人数（身元は見ない）
- **動き** — カメラのスナップショットを128x72のグレースケールにして、直前のフレームとの平均差分

顔検出だけだと遠くに立っている人を落とすため、動きを併用しています（実測で静止時は差分0.3〜0.6、動きがあると数倍に跳ねます）。どちらも反応しない状態が既定900秒続くと `xrandr --brightness 0.5` へフェードし、次の動き・顔で1秒以内に全輝度へ戻ります。

- 画像はディスクに残さず、直前1フレームとの比較にしか使いません
- 両方の信号が取れないときは **安全側**（全輝度のまま）
- 輝度を変えた直後5秒は動き判定を止め、自分の変化で起き続けるのを防ぐ

設定は systemd の `Environment=` で行います。

| 変数 | 既定 | 意味 |
|---|---|---|
| `PRESENCE_OUTPUT` | `HDMI-2` | 対象のRandR出力 |
| `PRESENCE_IDLE_SECONDS` | `900` | 暗くするまでの無反応時間 |
| `PRESENCE_DIM` | `0.5` | 暗くするときの輝度 |
| `PRESENCE_MOTION_DIFF` | `2.0` | 動きとみなす平均差分 |
| `PRESENCE_STATUS_URL` | `http://127.0.0.1:8093/status` | 顔検出の状態 |
| `PRESENCE_SNAPSHOT_URL` | `http://127.0.0.1:8082/snapshot.jpg` | 動き判定に使うスナップショット |

## room-survey

部屋の状態を「計測図」として描き、MJPEGで配信するサービスです。写真を加工するのではなく、解析結果を図にします。

| 信号 | 取り方 | 図への出方 |
|---|---|---|
| 明るさ | カメラフレーム | 等高線（部屋が地形になる） |
| 動き | フレーム差分 | 動いた場所にオレンジの輪 |
| 音 | ALSA 16kHz → FFT 56帯域 | 下部のスペクトラム、計測値 |
| 顔 | 顔検出の `/status` | 人数の表示（身元は見ない） |

- 出力は **正方形 720x720**。ライブタイルのカードがほぼ正方形なので、余白なく収まります
- カメラは正方形に中央クロップし、歪ませません
- 音は絶対値ではなく**その瞬間のスペクトルの形**で正規化します（静かな部屋でも死なないため）
- `GET /stream` がMJPEG、`GET /frame.jpg` が静止画、`GET /status` が計測値

### 日次レポートカード

1日1回、決まった時刻に数秒だけ「今日のまとめ」を表示します。

- 稼働時間（動きが閾値以上の分数）、24時間の活動バー
- いちばん賑やかだった時刻・静かだった時刻
- その日の最大音量、サンプル数
- 先週の同じ曜日との比

比べるには材料が要るので、**3時間分以上のデータがない日は時刻の比較を出しません**。その日トータル60分未満のうちは「COLLECTING」と表示します。

| 変数 | 既定 | 意味 |
|---|---|---|
| `SURVEY_REPORT_AT` | `18:30` | 表示する時刻（HH:MM） |
| `SURVEY_REPORT_SECONDS` | `25` | 表示秒数 |

MJPEGの各パートには `X-Frame-Sequence` と RFC3339 の `X-Frame-Timestamp` が必要です（アプリ側が検証します）。時刻は3秒以内でなければ捨てられます。

## office-stats

表示機がすでに測っている値を、1分単位の集約として残します。

| 列 | 内容 |
|---|---|
| `motion` / `motion_peak` | 動きの平均と最大 |
| `db` / `db_peak` | 音量の平均と最大 |
| `faces` | その1分で観測した最大の顔数 |
| `n` | その1分に入ったサンプル数（10秒ごと） |

**保存しないもの**: 画像、音声、顔の位置、個人の識別。行は「分」と集約値だけで、誰がいたかは残りません。

- 保存先: `~/office-stats/stats.db`（SQLite）
- 保持: 既定120日
- API: `/stats/today`、`/stats/week`（7x24グリッド）、`/stats/recent?minutes=N`、`/status`

計測図の下部にはこの週間グリッドが表示されます。データが溜まるまでは空セルのままなので、**数日動かして初めて意味が出ます**。「VS USUAL」もその曜日・時刻のサンプルが20分以上ないうちは `-` を表示します。

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
