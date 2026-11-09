# リモートアクセスのセットアップ

Kubernetes 1.25 以降で、Pod 内の Xvfb・XFCE・TigerVNC を使ったリモート
デスクトップを起動します。ローカルの `velink-desktop` は TCP 8080 の
`/vnc` に WebSocket 接続し、画面表示とキーボード・マウス操作を行います。
VNC の TCP 5900 は Pod 内の loopback 専用です。

この手順では `deploy/k8s/` を使用します。既存の `deploy/kubernetes/` は
別構成のため、同時に適用しないでください。以下のコマンドは、特記のない限り
リポジトリのルートで実行します。

## 前提条件

- Kubernetes 1.25+ と、そのクラスタに接続する `kubectl`。
- Namespace、ServiceAccount、Role、RoleBinding、Lease、ConfigMap、Pod、Service
  を作成できる権限。
- Docker などのイメージビルド環境と、ノードから取得できるコンテナレジストリ。
  ローカルクラスタではイメージの直接ロードも利用できます。
- クライアント用の Rust 1.85+。Linux では X11 または XWayland と
  X11 開発パッケージ（Debian/Ubuntu: `libx11-dev`）が必要です。
- ノードにデスクトップ用 CPU 250m・メモリ 512Mi、補助コンテナ用 CPU 25m・
  メモリ 32Mi の requests を確保できること。

現在の実装には認証・認可・TLS がありません。NodePort 30080 は VPN 内などの
信頼できるネットワークで使用し、ファイアウォールで接続元を制限してください。
クライアントは `ws://` のみ対応しています。安全な経路として、開発時は
`kubectl port-forward`、遠隔地からは VPN や SSH トンネルを利用できます。

## イメージの準備

```sh
docker build -f crates/velink-remote/Dockerfile -t velink-remote:dev .

# kind の例。対象クラスタ名が異なる場合は --name を指定する。
kind load docker-image velink-remote:dev
```

リモートクラスタでは、利用するレジストリへ push してください。

```sh
# registry.example.com は利用するレジストリに置き換える。
docker tag velink-remote:dev registry.example.com/velink-remote:v0.1.0
docker push registry.example.com/velink-remote:v0.1.0
```

push した場合は `deploy/k8s/pod.yaml` の `desktop.image` を同じタグに変更します。
プライベートレジストリでは namespace `velink` に認証 Secret を作成し、
Pod の `spec.imagePullSecrets` に指定します。補助コンテナの
`python:3.13-slim-bookworm` もノードから取得できる必要があります。
再現性が必要な環境では両イメージを digest で固定してください。

## Kubernetes へのデプロイ

```sh
kubectl config current-context

# Namespace を先に作成する。
kubectl apply -f deploy/k8s/namespace.yaml
kubectl apply -f deploy/k8s/pod.yaml
kubectl apply -f deploy/k8s/service.yaml

kubectl -n velink wait --for=condition=Ready pod/velink-remote --timeout=180s
kubectl -n velink get pods,services
kubectl -n velink get lease velink-remote-leader
```

`namespace.yaml` は `velink` namespace を作成します。以下ではすべて `-n velink`
を明示するため、kubectl の現在の namespace を変更する必要はありません。

`pod.yaml` は Pod に加え、リーダー選出用の ServiceAccount、Role、RoleBinding、
Lease、スクリプトを格納する ConfigMap を含みます。環境変数・ボリュームなど
Pod の変更できない項目を編集した場合は、次の手順で再作成します。
ConfigMap のスクリプト変更も、再作成によって反映します。

```sh
kubectl -n velink delete pod velink-remote --wait=true
kubectl apply -f deploy/k8s/pod.yaml
kubectl -n velink wait --for=condition=Ready pod/velink-remote --timeout=180s
```

単独の Pod のため、コンテナ停止時は `restartPolicy: Always` で再起動しますが、
Pod の削除やノード障害後に代替 Pod を自動作成するコントローラーはありません。
必要に応じて再適用してください。Pod の再作成でデスクトップの状態は失われます。

## 環境変数とリソース

| 環境変数 | マニフェストの値 | 用途 |
| --- | --- | --- |
| `POD_NAME` / `POD_NAMESPACE` | Downward API | サーバーログ用の Pod 名・namespace |
| `VELINK_LISTEN` | `0.0.0.0:8080` | WebSocket とヘルスチェックの待受 |
| `VELINK_DISPLAY` | `99` | Xvfb の display 番号。コロンは付けない |
| `VELINK_WIDTH` / `VELINK_HEIGHT` | `1280` / `720` | 固定解像度。積は最大 16,777,216 ピクセル |
| `VELINK_VNC_PORT` | `5900` | Pod 内の TigerVNC ポート |
| `VELINK_DESKTOP_COMMAND` | `startxfce4` | `dbus-run-session` 経由で実行するコマンド |
| `POD_UID` | Downward API | 補助コンテナのリーダー識別子の一部 |

環境変数は `pod.yaml` の各コンテナの `env` で設定します。
`POD_UID` はリーダー選出用で、起動ごとの UUID と組み合わせて使用します。
サーバー本体にリーダー選出用の環境変数はありません。

デスクトップの limits は CPU 2 コア・メモリ 2Gi、補助コンテナは CPU 100m・
メモリ 64Mi です。`/dev/shm` に最大 256Mi のメモリ上の `emptyDir` をマウントします。
共有メモリの使用量もメモリ消費に含まれるため、解像度や実行アプリに応じて
requests・limits・共有メモリの上限を合わせて調整してください。

## リーダー選出とヘルスチェック

リーダー選出は `coordination.k8s.io/v1` の Lease `velink-remote-leader` を使います。
`leader-election` 補助コンテナが 2 秒間隔で取得・更新し、有効期間は 15 秒です。
`resourceVersion` を維持した更新によって、同時取得の競合を検出します。
他の所有者の Lease は、内容が有効期間以上変わらないことを観測してから取得します。
正常終了時も Lease の解放は期限切れを待つため、再作成直後は待機する場合があります。

更新開始から 10 秒を過ぎるか API 操作に失敗すると、補助コンテナの `/readyz` が
失敗します。両コンテナの readiness が成功した Pod だけが Service の接続先になります。
ServiceAccount の権限は、この namespace の指定 Lease に対する `get`・`update`
だけです。API トークンと CA 証明書は補助コンテナにだけマウントします。
NetworkPolicy 等を使用する場合は、補助コンテナから API サーバーへの HTTPS を許可します。

既定の Pod は 1 台なので、通常はその Pod がリーダーになります。候補を追加する場合は、
Pod 名を変え、同じ namespace・ラベル・ServiceAccount・Lease を使用してください。
待機候補の Pod は `Running` でも `Ready` にはなりません。
この選出は Service の新規接続先を制御するもので、既存 WebSocket を強制切断する
フェンシングや、デスクトップ状態の同期は行いません。接続先が切り替わった場合は
クライアントを再起動して接続し直します。厳密な単一実行やセッションの無停止継続を
保証する構成ではありません。

デスクトップには HTTP `/healthz` の startup・readiness・liveness probe を設定します。
起動には最大約 60 秒の猶予を設け、その後は readiness を 5 秒間隔、liveness を
10 秒間隔で確認します。子プロセスの終了は本体が検出して終了し、コンテナが再起動します。
補助コンテナは別ポート 8081 の `/healthz` で選出ループの進行を確認します。
8081 と 5900 は Service では公開しません。

仕様の詳細は Kubernetes 公式の [Lease](https://kubernetes.io/docs/concepts/architecture/leases/)
と [probe の設定](https://kubernetes.io/docs/tasks/configure-pod-container/configure-liveness-readiness-startup-probes/)
を参照してください。

## ローカルクライアントから接続

開発時は次のコマンドを実行したターミナルを開いたままにします。

```sh
kubectl -n velink port-forward service/velink-remote 8080:8080
```

別のターミナルで実行します。

```sh
curl --fail http://127.0.0.1:8080/healthz
cargo run -p velink-desktop -- remote ws://127.0.0.1:8080/vnc
```

表示された XFCE デスクトップでキー入力・クリック・スクロールを確認します。
ウィンドウを閉じると切断し、同じ URL で再接続できます。同時接続は各 Pod につき
1 クライアントです。ブラウザで URL を開く方式ではありません。
`port-forward` は対象 Pod に直接転送するため、Service の readiness による
リーダー制御を迂回します。複数候補を動かす際は、以下の Service 経由で接続してください。

### NodePort

```sh
kubectl get nodes -o wide
# NODE_IP を VPN 等から到達可能なノードの IP に置き換える。
cargo run -p velink-desktop -- remote ws://NODE_IP:30080/vnc
```

Service のポートは 8080、ノード側のポートは 30080 です。kind などのローカル
クラスタでは NodePort をホストへ公開する追加設定が必要な場合があります。
到達できない場合は `port-forward` を使用してください。

### LoadBalancer に変更する場合

LoadBalancer 対応環境では、適用前に `service.yaml` の `spec.type` を
`LoadBalancer` に変更し、`ports[].nodePort` を削除して再適用します。
外部到達範囲はクラウド側の内部 LB 設定やファイアウォールで制限してください。

```sh
kubectl apply -f deploy/k8s/service.yaml
kubectl -n velink get service velink-remote --watch
# LB_ADDRESS を割り当てられた IP またはホスト名に置き換える。
cargo run -p velink-desktop -- remote ws://LB_ADDRESS:8080/vnc
```

## トラブルシューティング

まずイベントと各コンテナのログを確認します。

```sh
kubectl -n velink describe pod velink-remote
kubectl -n velink logs velink-remote -c desktop
kubectl -n velink logs velink-remote -c leader-election
kubectl -n velink get lease velink-remote-leader -o yaml
kubectl -n velink get endpointslices -l kubernetes.io/service-name=velink-remote -o yaml
```

| 症状 | 確認・対処 |
| --- | --- |
| `Pending` | イベントの `Insufficient cpu/memory`、ノードの taint や空き容量を確認する |
| `ImagePullBackOff` | イメージ名・タグ、kind へのロード先、レジストリ認証、補助イメージの取得可否を確認する |
| `Running` だが `Ready` にならない | desktop の起動ログ、Lease の所有者、補助コンテナのログを確認する。待機候補では正常な状態 |
| 選出ログに HTTP 403 | ServiceAccount と RoleBinding、指定 Lease への `get/update` 権限を確認する |
| 選出ログに HTTP 404 | `pod.yaml` を再適用して Lease の存在を確認する |
| 選出ログに HTTP 409 | 同時更新の競合は再試行される。続く場合は同じ Lease を更新している別アプリがないか確認する |
| API 接続がタイムアウト | API サーバーの可用性、NetworkPolicy、HTTPS 到達性を確認する。API 障害時は新規接続の受付を停止する |
| `CrashLoopBackOff` / `OOMKilled` | `kubectl logs ... -c desktop --previous` と終了理由を確認し、メモリ上限や解像度を調整する |
| Xvfb・TigerVNC が起動しない | Dockerfile の依存パッケージと display 番号、デスクトップコマンドを確認する |
| 接続拒否・タイムアウト | Ready 状態と EndpointSlice、URL のポート、VPN・ファイアウォールを確認する。port-forward で経路を切り分ける |
| HTTP 404 / ハンドシェイク失敗 | URL を `ws://接続先:ポート/vnc` にする。`http://` や `wss://` はクライアント非対応 |
| HTTP 409 | 既存のクライアントを閉じてから再接続する |
| 画面表示が遅い | Raw 形式の転送のため帯域を消費する。解像度を下げて Pod を再作成する |
| ローカルウィンドウが開かない | Linux の `DISPLAY`、X11/XWayland、X11 ライブラリを確認する |

IME、クリップボード同期、音声、動的な解像度変更は未対応です。
プロトコルと実装の詳細は [既存のリモートデスクトップ資料](remote-desktop.md) を参照してください。

## 削除

```sh
kubectl delete -f deploy/k8s/service.yaml
kubectl delete -f deploy/k8s/pod.yaml
# namespace 内の他のリソースもすべて削除してよい場合だけ実行する。
kubectl delete -f deploy/k8s/namespace.yaml
```
