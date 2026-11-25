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

- サーバー自体に認証・TLS はない。TLS 終端と認証は前段（Ingress + oauth2-proxy）が担う。
  クラスタ内の Service は ClusterIP のままとし、開発時は `kubectl port-forward` 経由で接続できる。
- TigerVNC は `127.0.0.1` のみで待受。WebSocket 接続は一度に 1 つ。
- 解像度は起動時固定。複数セッション、画面サイズ自動変更は未対応。
- キーボードは US 配列。IME/preedit、クリップボード同期、音声転送は未対応。
- 画面は非圧縮 Raw 形式。高解像度・遠距離接続の帯域最適化は今後の範囲。
- Pod 再作成時にデスクトップ状態は失われる。

プロトコル仕様: [RFB 3.8 (RFC 6143)](https://www.rfc-editor.org/rfc/rfc6143.html)、
サーバーオプション: [TigerVNC x0vncserver](https://tigervnc.org/doc/x0vncserver.html)。

## Windows クライアント

### インストール

GitHub Release の最新版から `velink_*-setup.exe`（NSIS インストーラー）をダウンロードして実行する。
`v*` タグの push 時に Release へ自動添付される。タグ前でも Actions の `Release` ワークフローの
成果物 `velink-windows-installer` から取得できる。インストール後はスタートメニューの
「velink」から接続し、「velink Settings」から設定を開く。

インストーラーは署名されていないため、初回実行時に Windows SmartScreen の警告が表示される。
「詳細情報」→「実行」で続行する。

### 接続先の指定と設定画面

コマンドライン引数 `velink-desktop remote ws://host:port/vnc` または
`velink-desktop remote wss://host/vnc` を指定すると、その URL が優先される。
URL を省略すると設定ファイルの `endpoint` が使われる。

引数なし、または `remote` のみで起動し、設定に `endpoint` が無い場合は
設定ウィンドウが自動で開く。**Save** で保存して接続し、**Cancel** または
ウィンドウを閉じると終了する。ウィンドウを起動できない環境ではコンソールで URL を尋ねる。
コンソールで入力した有効な URL も保存する。保存に失敗した場合は警告を出して接続を続ける。

設定済みの URL や Keycloak の項目は `velink-desktop settings`、または Windows の
スタートメニューの **velink Settings** から編集できる。この方法で開いた場合は保存しても接続を開始しない。
設定ウィンドウの **Endpoint** には `ws://…/vnc` または `wss://…/vnc` を入力する。
**Issuer**、**Client ID**、**Scope** は Keycloak の設定。保存できない場合は画面内にエラーが表示される。

設定ファイルは Windows では `%APPDATA%\velink\config.toml`、Linux では
`$XDG_CONFIG_HOME/velink/config.toml`（未設定なら `~/.config/velink/config.toml`）。
Linux で作成・更新したファイルの権限は 0600。設定ファイルは直接編集することもできる。

```toml
endpoint = "ws://host:port/vnc"
```

### wss:// と Keycloak ログイン（Device Flow）

公開ホスト（例: `wss://velink.kigawa.net/vnc`）には `wss://` で接続する。`wss://` のときだけ
Keycloak の Device Authorization Grant (RFC 8628) でログインし、WebSocket ハンドシェイクに
`Authorization: Bearer <access_token>` を付ける。

```sh
velink-desktop remote wss://velink.kigawa.net/vnc
```

初回手順:

1. 保存済みトークンが無いと、コンソールに URL と `Code: XXXX-XXXX` が表示され、ブラウザが自動で開く。
   ブラウザが開かない場合は表示された URL を手動で開く。
2. ブラウザで Keycloak にログインし、表示されたコードを入力（自動入力される場合もある）して承認する。
3. 承認後、クライアントが自動で接続を続ける。

2 回目以降は保存済みトークンを使い、期限が近いと `refresh_token` で自動更新する。更新に失敗すると
再ログインする。サーバーが 401/403 を返した場合は保存済みトークンを破棄し、再実行でログインを促す。

トークンの保存先: `%APPDATA%\velink\token.json`（Linux: `$XDG_CONFIG_HOME/velink/token.json`、
未設定なら `~/.config/velink/token.json`、権限 0600）。`access_token` / `refresh_token` は画面やログに出力しない。
設定画面の **Delete saved token** で `token.json` を削除できる。結果は画面内に表示され、
次回の `wss://` 接続時にログインを求められる。

Keycloak 設定（public client、`client_secret` は不要。環境変数が設定ファイルより優先）:

| 項目 | 環境変数 | config.toml キー | 既定値 |
| --- | --- | --- | --- |
| issuer | `VELINK_ISSUER` | `issuer` | `https://user.kigawa.net/realms/develop` |
| client_id | `VELINK_CLIENT_ID` | `client_id` | `velink-desktop` |
| scope | — | `scope` | `openid offline_access` |

Keycloak 側でクライアントの OAuth 2.0 Device Authorization Grant を有効にしておくこと。

### 注意

- `ws://` は平文・認証なしで、LAN など信頼できる経路専用。公開ホストには `wss://` を使うこと。
- キーボードは US 配列を前提とする。

### インストーラーのビルド（開発者向け）

```powershell
cargo install cargo-packager --locked
cargo build -p velink-desktop --release --locked
cargo packager -p velink-desktop --release --formats nsis
```

インストーラーには `velink-desktop.exe` と `velink-settings.exe` が含まれる。
