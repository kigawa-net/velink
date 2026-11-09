# Kubernetes リモートデスクトップ

Kubernetes 1.25+、Rust 1.85+ を対象にした単一セッションの開発用リモートデスクトップ。
Pod 内で Xvfb → XFCE → TigerVNC scraping server を起動し、RFB 3.8 を
WebSocket のバイナリメッセージで転送する。クライアントは Raw エンコーディングの
32-bit RGB を専用ウィンドウに表示し、キー・ポインター・スクロールを送信する。
既存の `firefox <url>` / `chromium <url>` コマンドは従来どおり利用できる。

## ビルドと配置

```sh
cargo build --workspace
cargo test --workspace
# リポジトリルートで実行
docker build -f crates/velink-remote/Dockerfile -t velink-remote:dev .
# 使用クラスタにイメージをロードするか、レジストリへ push して Deployment の image を変更する。
# kind の場合:
kind load docker-image velink-remote:dev
kubectl apply -f deploy/kubernetes/
kubectl rollout status deployment/velink-remote
kubectl port-forward service/velink-remote 8080:8080
```

別ターミナルのローカル PC で接続する:

```sh
cargo run -p velink-desktop -- remote ws://127.0.0.1:8080/vnc
```

Linux のクライアントは X11 または XWayland が必要。
ビルド時には X11 開発パッケージ（Debian/Ubuntu: `libx11-dev`）を用意する。
Windows でも同じコマンドを使用できる。

表示された XFCE デスクトップでターミナルを開き、文字入力、クリック、スクロールを
確認する。ウィンドウを閉じると接続を終了する。Pod のデスクトップは存続するので
同じ URL に再接続できる。同時に別クライアントが接続すると HTTP 409 を返す。
接続失敗は標準エラーに表示する。キー/ボタンはフォーカスを失った際に解放する。

## 設定

| 環境変数 | 既定値 | 内容 |
| --- | --- | --- |
| `VELINK_LISTEN` | `0.0.0.0:8080` | WebSocket / health HTTP 待受 |
| `VELINK_DISPLAY` | `99` | Xvfb の display 番号 |
| `VELINK_WIDTH` / `VELINK_HEIGHT` | `1280` / `720` | 固定解像度（最大 16 megapixels） |
| `VELINK_VNC_PORT` | `5900` | Pod 内の loopback VNC ポート |
| `VELINK_DESKTOP_COMMAND` | `startxfce4` | dbus-run-session で起動するデスクトップコマンド |
| `POD_NAMESPACE` / `POD_NAME` | `default` / 未指定 | Downward API 由来のログ用 Pod 情報 |

`/healthz` はデスクトップ起動後に応答する。子プロセスが終了するとサーバーも終了し、
Kubernetes が Pod のコンテナを再起動する。SIGTERM / Ctrl+C で所有する子プロセスを終了する。
クラスタ API 操作や RBAC は不要。Pod の作成はマニフェストで行う。

## 範囲と制約

- 認証・認可・TLS は未実装。Service は ClusterIP のままとし、開発時は
  `kubectl port-forward` 経由で接続する。公開 Ingress / LoadBalancer は用意しない。
- TigerVNC は `127.0.0.1` のみで待受。WebSocket 接続は一度に 1 つ。
- 解像度は起動時固定。複数セッション、画面サイズ自動変更は未対応。
- キーボードは US 配列。IME/preedit、クリップボード同期、音声転送は未対応。
- 画面は非圧縮 Raw 形式。高解像度・遠距離接続の帯域最適化は今後の範囲。
- Pod 再作成時にデスクトップ状態は失われる。

プロトコル仕様: [RFB 3.8 (RFC 6143)](https://www.rfc-editor.org/rfc/rfc6143.html)、
サーバーオプション: [TigerVNC x0vncserver](https://tigervnc.org/doc/x0vncserver.html)。
