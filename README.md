# velink

Kubernetes 上のサーバー PC へフルデスクトップ操作でリモートアクセスするツール。

## 機能

- Kubernetes 上の VNC サーバー（Xvfb + XFCE + TigerVNC）を WebSocket 経由で操作
- RFB 3.8 プロトコル対応の VNC クライアント
- 1 Pod = 1 クライアント同時接続

## クイックスタート

```sh
cargo run -p velink-desktop -- remote ws://接続先:8080/vnc
```

詳細は [remote-access.md](docs/remote-access.md) を参照。

## プロジェクト構成

| パッケージ | 説明 |
| --- | --- |
| velink-core | コア機能、セッション管理 |
| velink-remote | VNC サーバー、WebSocket ブリッジ、Dockerfile |
| velink-desktop | VNC クライアント（minifb 描画） |
| wayland-compositor | Wayland コンプォジター |
