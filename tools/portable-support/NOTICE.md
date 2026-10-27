# Third-party notices for the public preview

開発中のv0.5.0と、公開済みpre-release `v0.4.1-preview.1` の第三者通知。アプリのオリジナル部分はGPL-3.0-or-later、第三者部分は各原ライセンスに従う。各権利者の著作権表示とライセンス本文を保持する。v0.5.0の包装・総合検証・公開は準備中で、以下の旧版の配布情報を新しい版の完成証明とは扱わない。

v0.5.0は本プロジェクトで実装したUSB/LSB復調と任意の変調方式推定を追加する。追加の機械学習モデルや依存ダウンロードは必要とせず、下記driver・runtime・crateのライセンス方針は継続する。推定は候補と観測根拠を示す補助で、方式・VFO Hz・受信中心を自動変更せず、校正済み判定精度を主張しない。操作と制約は配布案内のSSB／方式推定の説明を参照する。

- **RTL-SDR Blog V1.4.0**: `rtlsdr.dll`は固定タグV1.4.0のソースから作成したWindows x64版。RTL-SDRのCソースはGPL-2.0-or-later、一部buildファイルはGPL-3.0-or-later。GPL-3.0-or-laterの本アプリと配布する。原COPYING、固定ソースarchive、ビルド手順を対応ソースへ保全する。
  - [公式タグのソース](https://github.com/rtlsdrblog/rtl-sdr-blog/tree/V1.4.0)
- **libusb 1.0.30**: 公式source tarballを固定して`rtlsdr.dll`へ静的リンク。LGPL-2.1-or-later。原COPYING、全source、MSVC runtime選択の2行patch、適用・ビルド手順を対応ソースへ含める。C/header本体は変更していない。別のlibusb DLLを同梱する構成ではない。
  - [公式安定版1.0.30](https://github.com/libusb/libusb/releases/tag/v1.0.30)
- **Microsoft Visual C++ runtime**: CRTは`/MD`の動的リンク。公開ZIPには`vcruntime140.dll`、UCRT/API-set、その他Windows system DLLを同梱しない。利用者がMicrosoft公式のVisual C++ v14 Redistributable（x64）を別途導入する。アプリのビルドに用いたMSVCと同じかそれより新しい対応runtimeが必要。Microsoftの利用条件が適用され、本アプリのGPLへ変更しない。
  - [Microsoft公式x64ランタイム](https://aka.ms/vc14/vc_redist.x64.exe)
  - [Microsoftの対応版・導入案内](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist?view=msvc-170)
- **Rust standard library**: 使用したtoolchainの`COPYRIGHT-library.html`と原ライセンス本文を同梱する。
- **Rust crates**: Windows x64の依存グラフ（build/proc-macro依存を含む）に対応するcrateの原ライセンス・著作権通知を`licenses/rust-crates/`へ保全する。配布物の一覧・宣言は`rust-crates.json`を参照。対応ソースZIPには`Cargo.lock`に固定された全66 crateをvendorするため、他OS向けを含むソースも取得できる。

[同じリリースの対応ソースZIP](https://github.com/ReiA-O/rtl-sdr-spectrum-studio/releases/tag/v0.4.1-preview.1) は、自作部分・全66 Rust crate・RTL-SDR/libusbの固定source archives・patch・ビルド手順を含む。source ZIP内の`SOURCE_BUILD.md`と`third-party-driver/README.md`から再ビルドできる。archiveとDLLのSHA256・toolchainはdriver用`BUILD-MANIFEST.json`に記録する。

公開済みv0.4.1はアプリ内部0.4.1／パッケージ0.4.1-public-preview.1。driver・ランタイム方針・全66crateの対応source提供はv0.4.0-preview.1から変更しない。対応source ZIPのトップは`RTL-SDR-Spectrum-Studio-0.4.1-source/`で、`SOURCE_BUILD.md`とdriver recipeを入口にする。 軸のドラッグ/左右キーは影響VFOの自動停止を事前承認済みとして行い、一般手入力の確認は保持する。新規VFOは操作実行時点の中心へ配置して選択する。これらのUI変更でdriver/sourceライセンス条件は変更しない。

v0.5.0のアプリ内部版は0.5.0、候補パッケージの既定は0.5.0-dev。対応sourceのトップは`RTL-SDR-Spectrum-Studio-0.5.0-source/`を予定し、新しい実装・UI・試験・文書を含めて照合する。最終の名前・hash・検証結果は確定後の配布案内に従う。既存v0.4.1のReleaseとassetは保持する。

この公開プレビューで使うDLLは192,000 bytes、SHA256 `761de6ef2f57047f1dfbbf2346cb53247b2974f5d1fb11b64f9498b650cafc05`。libusbはstatic、CRTはdynamic。元の公式prebuilt DLLを再配布する構成から変更した。

WinUSB USBドライバーはLIVE受信時に別途必要。パッケージはUSBドライバーの導入・変更や受信開始を自動実行しない。署名付きinstaller、独立したクリーンPC・屋外での受入れは未検証。

## 過去の構成

2026-10-27のprivate portable開発版では、未変更のRTL-SDR Blog公式x64 prebuilt DLLと正規VC/Redist由来の`vcruntime140.dll`を同梱した。その時点では本体の公開ライセンスが未選定で、公式DLL内に含まれるlibusbの正確な版・ビルド設定も未確認だった。旧成果物と履歴は保持する。公開プレビューでは固定ソースからのビルドへ切り替えて対応ソースを提供し、Microsoft runtimeを外部前提とする。旧prebuilt内のlibusb版は推測していない。
