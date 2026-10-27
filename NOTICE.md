# RTL-SDR Spectrum Studio — Notices

オリジナルコード・文書・同梱デモは、著作権が及ぶ範囲で **GPL-3.0-or-later** を適用します。本文と条件は [LICENSE.md](LICENSE.md) と [LICENSE](LICENSE) を参照してください。第三者の著作権・原ライセンス・通知は維持します。

開発中のアプリ内部版 `0.5.0` はUSB/LSB復調と任意の変調方式推定を追加する候補です。包装・総合検証・公開は準備中で、最終asset名・hashや合格件数は未確定です。追加機能の仕様と限界は [MODULATION.md](docs/MODULATION.md) に記載します。候補と観測根拠は受信補助であり、方式の自動切替や校正済み判定精度を意味しません。

公開済みの `v0.4.1-preview.1` はアプリ内部版 `0.4.1`、Windowsパッケージ版 `0.4.1-public-preview.1` のpre-releaseです。[リリースページ](https://github.com/ReiA-O/rtl-sdr-spectrum-studio/releases/tag/v0.4.1-preview.1) には、Windows x64用ZIPと、アプリ・全66 Rust crate・RTL-SDR/libusbの固定ソース・patch・手順を含む対応ソースZIPを用意します。これらはv0.4.1の成果物と履歴として保持し、v0.5.0の検証済み成果物とは扱いません。

v0.4.1の変更は音声の状態表示・終了操作・周波数表示、軸操作の受信中心移動と新規VFOの中心配置を対象とします。軸操作では影響VFOを確認ダイアログなしで自動停止し、一般手入力のD24確認は維持します。新しいVFOは操作の実行時点の中心へ追加して選択し、既存VFOの設定は保持します。GPL方針、driver、Microsoft runtimeの別導入、全66 Rust crateの対応source提供はv0.4.0-preview.1から維持します。新しい実行物の検証結果は版ごとに記録します。

v0.5.0のSSB／推定は本プロジェクトの実装で、追加の機械学習モデルや依存ダウンロードを必要としません。GPL方針、既存driverと全66 crateの原通知・対応ソース提供、Microsoft runtimeの別導入は継続します。搬送波基準の確認を保存設定からtrueへ復元せず、推定でVFOや受信中心を自動変更しません。

- RTL-SDR Blog V1.4.0とlibusb 1.0.30からWindows x64の`rtlsdr.dll`をソースビルドします。libusbは静的リンク、Microsoft CRTは動的リンクです。
- MicrosoftランタイムやWindows system DLLは公開ZIPに同梱しません。初回起動前に [Microsoft公式Visual C++ v14 Redistributable（x64）](https://aka.ms/vc14/vc_redist.x64.exe) を利用してください。Microsoftの利用条件が適用されます。
- Rust標準ライブラリーと依存crateの原ライセンス・著作権通知を配布物へ保全します。詳しい出典は [第三者通知](tools/portable-support/NOTICE.md) にあります。
- AFGで生成して収録した同梱IQの条件・再生方法は [デモ案内](demo/README.md) にあります。通常受信した個人の録音データは同梱しません。

再ビルド・配布構成は [PACKAGING.md](docs/PACKAGING.md)、検証済みの範囲と未検証部分は [VALIDATION.md](docs/VALIDATION.md) を参照してください。

2026-10-27以前のprivate開発版では公開ライセンスの選定が未完了で、公式prebuilt DLLとMicrosoftランタイムを同梱する構成でした。その経緯はREADMEとPACKAGINGの配布履歴に残し、この公開pre-releaseの構成と区別します。
