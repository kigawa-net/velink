# Velink 全体アーキテクチャ設計 v2.0 追記

本書は、全体アーキテクチャ設計 v2.0 に対する追記案です。既存の第17章「開発ロードマップ」に続く章として追加します。MVPの詳細は第20章、以降は第21〜23章に相当します。

## 20. External Browser MVP の範囲と完了条件

最初の実用MVPは、Local Native Runtime上でFirefoxまたはChromiumを起動し、そのトップレベルウィンドウをVelinkのタブとして表示・操作できる状態とします。対象は新規に起動したウィンドウに限定し、既存ブラウザプロセスへの任意ウィンドウ取り込みや、ブラウザ内部タブの同期（L2以上）は対象外です。

### MVPに含める機能

- Browser Shellからブラウザ種別とURLを指定して新しい外部ブラウザウィンドウを起動する。
- 起動要求と最初に生成された管理対象トップレベルウィンドウを一意に対応付ける。
- Window RegistryのウィンドウSurfaceをVelinkのSurfaceとして表示する。
- Velinkのフォーカス対象へキーボード、ポインター、スクロール入力を配送する。
- 複数の外部ウィンドウを別タブとして保持し、2面のSplit Viewで同時に表示・操作する。
- タブを閉じたときの挙動（ブラウザウィンドウ終了、Session維持など）をポリシーとして定義し、MVPでは既定で当該ウィンドウを終了する。
- ブラウザまたはRuntime Agentの異常終了を検出し、Shellの操作継続と再起動・終了の選択を可能にする。

### MVPの完了条件

1. FirefoxとChromiumをそれぞれ起動し、別のVelinkタブで表示できる。
2. 各タブでURL入力、スクロール、クリック、文字入力が対象ウィンドウに届く。
3. 2つのタブをSplit Viewに配置し、両方を操作できる。
4. Popup、Context Menu、ファイル選択などの子ウィンドウが親ウィンドウとの関係を保ち、入力・表示が破綻しない。
5. ブラウザの終了や異常終了が他タブやBrowser Shellを停止させない。
6. SurfaceのResizeと再接続後に、古いフレームや入力が新しいWindowへ混入しない。

この段階ではPodman、exkes Remote、ブラウザ内部タブ同期、独自Web Engineのページ表示を完了条件に含めません。将来の実装目標とは分けて進捗を評価します。

## 21. Window Binding と Surface 表示経路

外部ウィンドウの識別から表示までの関連は、以下のようにします。

```text
LaunchRequestId
    → RuntimeInstanceId
    → GuiSessionId
    → ManagedWindowId + generation
    → SurfaceId + display_epoch
    → ContentId
    → TabId
```

`LaunchRequestId`はBrowser Coreが起動要求ごとに生成します。Runtime Agentは起動要求をWayland Compositorへ関連付け、対象アプリケーションから生成された最初の管理対象トップレベルウィンドウを報告します。Compositorはウィンドウごとに`ManagedWindowId`と単調増加する`generation`を割り当てます。Surface ManagerはWindow Registryのイベントを受けてSurfaceを生成し、Browser CoreがContentとTabへ関連付けます。PIDやタイトル文字列だけを恒久的な識別子として使用しません。

### Surfaceの取得・転送

- Local NativeではVirtual Wayland Compositorが管理するSurface Bufferをローカル表示経路へ渡します。GPUバッファ共有が利用可能なら共有バッファと明示的な同期機構を使い、利用できない環境ではコピー経路へフォールバックします。
- Remote RuntimeではVDSP Videoチャネルでフレームを転送し、クライアント側でVideo Textureとして取り込みます。
- ローカルの共有GPUバッファ経路とリモートの映像転送経路は別のTransport Adapterとして扱い、SurfaceのID、Damage、Resize、Frame Ackなどの論理契約を共通化します。
- GPU API、バッファ形式、同期方式、色空間、解像度・スケールはBackend能力としてネゴシエーションします。未対応形式やデバイス喪失時のCPUコピー経路を用意します。

### ウィンドウと子Surface

起動要求で作成された最初のトップレベルWindowを、その要求のPrimary Windowとします。後続の独立トップレベルウィンドウは新しいContent/Tabとして登録します。PopupとContext Menuは親Windowに紐付け、同じTabの一時Surfaceとして合成します。ファイル選択などのModal Windowは親子関係を維持し、親Tab内で表示して入力をModal Windowへ限定します。アプリが関連付け不能なウィンドウを作成した場合は孤児Surfaceとして表示せず、Runtime Agentへ拒否または診断イベントを返します。

## 22. Input と Focus の規則

入力対象はBrowser Coreが管理する明示的なFocus Stateで決定します。状態は少なくとも`ShellFocused`、`ContentFocused(TabId, SurfaceId, focus_epoch)`、`ModalFocused(...)`を持ちます。Tab切替、Modal表示、Surface再生成、Runtime再接続では`focus_epoch`を更新し、古いEpochを持つ入力イベントを破棄します。

- Browser Shellのグローバル予約ショートカットは、Contentへ転送する前に処理します。MVPでは制御復帰とPalette表示に必要な最小限の組み合わせだけを予約します。
- 予約キー以外は、フォーカス中のContentへ配送します。Ctrl+Lなどブラウザ固有のキーは、Velink側が予約しない限り外部ブラウザへ届きます。
- ポインター入力は、Native OverlayのHit Test後、最前面のContent Surfaceへ配送します。PopupやModalがある場合は、規則に従ってそのSurfaceを優先します。
- ポインターCapture中も予約された制御復帰操作はShellが処理します。Capture終了・フォーカス喪失時には、仮想Seatへボタン解放とキー解放を送り、押下状態が残らないようにします。
- IMEはFocused Contentに結び付けます。Preedit、Commit、候補位置を扱えないBackendでは、明示的に非対応を報告し、通常キー入力へ誤変換して配送しません。
- キーイベントには`connection_epoch`と`focus_epoch`を含め、世代が一致しない入力を受け付けません。

予約キーの具体的な組み合わせはOSの標準ショートカットとの衝突を確認して決定し、設定可能にします。アプリケーションが予約キーを必要とする場合の代替手段も提供します。

## 23. Runtime / Session / Connection の状態と復旧

Runtime、GUI Session、接続、Window、Streamは別々のライフサイクルを持ちます。Browser Coreはそれぞれの状態を統合表示しますが、所有元の状態を混同しません。

| リソース | 主な状態 | 所有・権威 |
| --- | --- | --- |
| Runtime | `Creating → Ready → Stopping → Stopped`、`Failed` | Runtime Manager / Runtime Provider |
| GUI Session | `Starting → Running → Terminating → Terminated`、`Failed` | Runtime Agent |
| Connection | `Disconnected → Connecting → Connected → Reconnecting`、`Failed` | VDSP Client / Agent |
| Window | `Created → Mapped → Unmapped → Destroyed` | Virtual Wayland Compositor |
| Stream | `Negotiating → Active → Paused → Closed`、`Failed` | Surface/VDSP Adapter |

Runtime Providerが権威を持つRuntime状態と、クライアントが観測するConnection状態を分離します。切断だけではSession終了と判断せず、再接続可能なSession IDがあれば再接続を試みます。Runtime再起動後はRuntime Providerが新しい`runtime_generation`を発行し、旧世代のWindow、Stream、Inputを無効にします。

### 世代と永続化

- `runtime_generation`はRuntime再生成時に更新します。
- `session_epoch`はGUI Sessionの新規作成時に更新します。
- `connection_epoch`は接続確立ごとに更新します。
- `display_epoch`はSurfaceの再生成または表示構成の再初期化時に更新します。
- `stream_epoch`はVideo Streamの再ネゴシエーション時に更新します。
- `focus_epoch`は入力フォーカスの遷移時に更新します。

各イベントは該当する世代値を持ち、受信側は現在値と一致しないイベントを破棄します。どのEpochを必須とするかはメッセージ種別ごとにVDSP/IPCスキーマで規定します。

永続化対象は、WorkspaceのLayout、TabとContentの参照、Runtime Providerが再接続に必要とする不透明なSession参照、およびユーザー設定です。生きたプロセスのメモリ状態や一時的なConnection IDは永続化しません。認証資格情報は通常のWorkspaceデータと分離した安全な資格情報ストアに保存し、失効可能にします。

再起動時はWorkspaceを復元した後、保存されたSession参照の有効性をRuntime Providerへ問い合わせます。有効なら新しいConnectionを確立し、Window RegistryとStreamを再同期します。無効または期限切れならContentを停止済みとして表示し、再起動または閉じる操作を提示します。再同期完了までは古い映像を入力可能な状態として扱いません。
