# Windows x64配布フォルダーの作成

これは開発PCで明示実行する手順です。受取側PCはMicrosoft公式Visual C++ v14ランタイム（x64）を用意してZIPを展開し、起動します。Rust/Python/Node/Build Toolsを必要としません。作成側にはRust MSVCとMicrosoft C++ Build Tools、Windows PowerShellが必要です。自動USBドライバー導入・受信開始・機器設定変更は行いません。

## 1. テストとビルド

```powershell
cargo test --manifest-path native\Cargo.toml --all-targets
node tests\ui-static-boundaries.cjs
node tests\ui-reception-boundaries.cjs
node --test tests\frontend-ui.test.cjs
```

配布バイナリーへ開発PCのソースパスを埋めないため、パスをremapしてビルドします。空白を含むパスを扱えるよう、区切り付きのRust flagsを使用します。これらの環境変数は現在のPowerShell内だけに設定します。

```powershell
$repo = (Get-Location).Path
$env:CARGO_ENCODED_RUSTFLAGS = @(
    "--remap-path-prefix=$repo=/workspace"
    "--remap-path-prefix=$env:USERPROFILE=/user"
    "--remap-path-prefix=$($repo.Replace('\','/'))=/workspace"
    "--remap-path-prefix=$($env:USERPROFILE.Replace('\','/'))=/user"
) -join [char]31
cargo build --manifest-path native\Cargo.toml --release --locked --bin rtl-spectrum-studio --target-dir native\target-portable
```

環境によりコンパイラーがWindowsの区切りでパスを記録する場合があります。完成exeのUTF8/UTF16文字列検査を行い、必要なら同じ対応をバックスラッシュ表記にも追加してください。remapフラグだけでプライバシー確認済みとはしません。

## 2. 公開プレビューの入力

公開pre-releaseのタグは **`v0.4.0-preview.1`**、パッケージ版は **`0.4.0-public-preview.1`**、アプリ内部版は **`0.4.0`** です。[同じリリース](https://github.com/ReiA-O/rtl-sdr-spectrum-studio/releases/tag/v0.4.0-preview.1) にWindows x64のデモ同梱ZIPと、全対応ソースZIPを用意します。ファイル名・サイズ・SHA256は各リリースassetとmanifestで照合してください。

公開版に用いるuser-spaceの`rtlsdr.dll`は、RTL-SDR Blog V1.4.0の固定ソースとlibusb 1.0.30の公式tarballからMSVCでビルドしたものです。libusbを静的リンクし、Microsoft CRTは`/MD`で動的リンクします。承認済みDLLは192,000 bytes、SHA256 `761de6ef2f57047f1dfbbf2346cb53247b2974f5d1fb11b64f9498b650cafc05` です。Windows USBドライバーの導入・変更は行いません。

対応ソースZIPのトップは`RTL-SDR-Spectrum-Studio-0.4.0-source/`です。

- 自作コード・UI・テスト・ライセンス。
- `Cargo.lock`に固定した全66 crateのvendorソースと原通知。
- `third-party-driver/`の6ファイル：RTL-SDR固定source ZIP、libusb公式source tarball、MSVC runtime選択のpatch、`build-driver.ps1`、`BUILD-MANIFEST.json`、README。
- アプリのオフラインビルドはトップの`SOURCE_BUILD.md`、driverの抽出・patch・ビルドは`third-party-driver/README.md`。

通常のGitHub自動生成source archiveはRustのvendor依存を含まないため、ネット接続なしでの再ビルドには別添の対応ソースZIPを使用します。

```powershell
# 対応ソースZIPを展開したトップで、SOURCE_BUILD.mdの前提に従って実行
cargo build --release --frozen --jobs 2 --manifest-path native/Cargo.toml --target-dir target
# Driverだけをソースからビルドする場合（新規の短い出力先を指定）
.\third-party-driver\build-driver.ps1 -BuildDirectory C:\rtl-build
```

ソース再ビルドは成功を確認していますが、timestampsやtoolchain差によりDLLのbyte hashは変わる場合があります。下記packagerはこの公開previewで承認したDLL hashと6つの対応source/build材料を固定して検査します。任意の再ビルドDLLをそのまま包装する入口ではありません。別buildを配布する場合は、対応manifest・入力pinの更新と、依存・ライセンス・プライバシー・実行経路の再検証が必要です。

packagerの原通知・sourceキャッシュとWindows x64依存metadataを用意する場合は、既存の補助コマンドを明示実行します。

```powershell
$prepared = .\tools\prepare-packaging.ps1 | ConvertFrom-Json
```

この補助は`provenance.json`に記録された公式URL・hashの材料と、旧private包装で使う公式prebuilt DLLのcacheも取得します。既存内容が異なる場合は上書きせず停止します。公開包装では次項の`-PublicRelease -DriverDllPath -DriverSourceDirectory`を明示し、source-built DLLを選びます。metadataには開発PCの絶対pathが入るためGitや対応ソースへ追加しません。この工程はZadigの実行、USBドライバー導入、受信開始を行いません。

## 3. 公開用の新規フォルダーとZIP

次の`$driverDll`と`$driverSources`には、この公開previewで承認したDLLと正確な6ファイルの場所を指定します。`$driverSources`は対応ソースZIP内の`third-party-driver/`に相当します。

```powershell
$binary = 'native\target-portable\release\rtl-spectrum-studio.exe'
$hash = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash
$driverDll = Read-Host '承認済みsource-built rtlsdr.dllのパス'
$driverSources = Read-Host '承認済み6ファイルを含むdriver sourceフォルダー'
.\tools\package-studio.ps1 -BinaryPath $binary -ExpectedBinarySha256 $hash -PackageVersion 0.4.0-public-preview.1 -CargoMetadataPath $prepared.cargo_metadata_path -PublicRelease -DriverDllPath $driverDll -DriverSourceDirectory $driverSources
```

公開ZIPに`vcruntime140.dll`やWindows system DLLを同梱しません。受取側は初回起動前に [Microsoft公式Visual C++ v14 Redistributable（x64）](https://aka.ms/vc14/vc_redist.x64.exe) を導入または更新します。ビルドに用いたMSVCと同じかそれより新しい対応版を使います。[Microsoftの導入案内](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist?view=msvc-170) を参照してください。アプリ・起動scriptからruntimeのダウンロードやインストールは行いません。保存IQ再生だけでもアプリ用runtimeは必要です。

同名成果物を置換せず、`dist/`へ時刻付きで新規作成します。元の`data/`は読まず、個人情報のない`presets/fm-measurement.json`を新しいsettingsの初期値へコピーします。別プリセットを使う場合は`-PresetPath`で明示します。Rust標準ライブラリーの通知の場所を変えた場合は`-RustCopyrightPath`を指定できます。

v0.4.0の初期プリセットは中心DC補正ONです。中心付近の実信号にも影響し、処理中のレベルは相対表示になります。中心AM局は搬送波が抑えられて包絡復調が崩れる場合があるため、OFF比較か、受信中心を局からずらしてVFOで合わせる運用を使います。RF感度／S/N改善ではありません。原IQ保存は補正前です。パッケージ生成の成功と、[VALIDATION.md](VALIDATION.md) の製品受入れは別の確認です。

PE依存・x64・承認exe/DLLのSHA256・固定source/build材料・全ZIP entryのhashを検査します。公開モードは外部runtime前提をmanifestへ記録し、GPL本文と第三者通知を同梱します。`-ForbiddenText`へ公開してはいけない文字列の配列を渡すと、全同梱ファイルをUTF8/UTF16LE/BEで検査してhit時に停止します。秘密文字列そのものをソースやmanifestへ保存しないでください。

Windows ZIPとは別に、全対応ソースZIPも同じpre-releaseへ添付します。アプリ本体、全66 crate、driver source/patch/recipe、原通知の欠落がないことを確認し、Gitの通常source ZIPだけで代用しません。

### AFG録音IQデモの同梱

明示承認されたAFG実録IQと対応プリセット・案内を`demo/`へ用意しています。`demo/manifest.json`の正確なファイル名・サイズ・SHA256で確認して同梱します。manifestのschemaは`spectrum-studio-distribution-demo-1`です。`-DemoManifestPath`でmanifestを指定でき、未指定時はリポジトリの`demo/manifest.json`が存在すれば使います。存在しない時に個人の録音フォルダーを探してコピーする動作はありません。

許可するのは`demo_afg_fm_32mhz.sigmf-meta`／`demo_afg_fm_32mhz.sigmf-data`、`AFG_FM_32MHz_demo.json`、デモ`README.md`の4ファイルです。IQ pairは配布先の`data/recordings/`直下、プリセットは`data/settings/presets/`、案内と監査用manifestは`demo/`へ配置します。相対パスの逸脱・未知の出力先・重複・サイズ／hash不一致は拒否し、元の`data/`全体は読み込みません。Gitも承認した2つのSigMF pathだけを例外とし、rawファイルへbinary属性を指定してbyte hashを保ちます。

旧デモ同梱版の拡張時にはpackage契約67件・export10件、当時の最終デモZIPの全entry hash・private文字列検査・自己診断が成功しました。受信DLLを無効化した模擬状態で当時の最終ZIPの再生14項目も成功しています。実GUI9項目は案内追記前の候補で確認し、当時の最終版とexe／IQ／metadata／presetは同一hashです。本体exeはv0.4.0と同じで、デモ同梱のためのコード再ビルドはしていません。

同梱後は「プリセット」から`AFG_FM_32MHz_demo`を読み込み、**設定画面右上の×で閉じてから**「保存データ」へ進みます。デモの「IQ再生」→「再生開始」後に音声をONにします。VFO32 MHz・WFM200 kHz、手動FFT4096点・実効RBW375 Hzの再生用設定と、従来の測定用初期設定は別のプリセットとして保持します。個人の通常受信記録・校正情報・機器IP／シリアル・ローカルパスは同梱対象にしません。

## 4. 別工程の確認

`--self-check`でルート検出、DLL読み込み、内蔵UIを確認できます。受信・サーバー・録音を開始しません。結果はパッケージ外へ保存し、完成ZIPのhashを保ちます。実画面・音声・別PC・屋外受信は、それぞれ別に確認してください。

デモの固定4ファイルとmanifestはバイト列で検査します。Git取得時もmanifestと一致するよう、IQはbinary、同梱metadata・preset・README・manifestは固定pathの-text属性で改行変換を抑止します。通常sourceの改行方針は維持します。

## 配布履歴と今回の変更

2026-10-27のprivate版では、RTL-SDR Blog V1.4.0の公式`Release.zip`からx64 DLLを取得し、原ZIPとのbyte一致を確認しました。`vcruntime140.dll`は正規Visual StudioのVC/Redistから同梱しました。その時点で公式DLL内のlibusbの正確な版・build由来は未確認、アプリの公開ライセンスも未選定でした。旧privateモードの原ZIP照合は互換用に保持しています。

同日作成の旧デモZIP `RTL-SDR-Spectrum-Studio-0.4.0-dev-demo-windows-x64-20261027-210450-328.zip`（11,296,451 bytes、SHA256 `d04cde25fc4e250837dd80ff4a5d5c19838f40261dd5906428aba0ec967892de`）はprivate draftとして保全しました。これら旧版の結果を、公開previewの配布hash・受入れ結果へ読み替えません。

公開previewではGPL-3.0-or-laterの選定、固定RTL/libusb sourceからのDLL作成、CRT動的リンク・runtime非同梱、全対応sourceZIPの提供へ変更しました。旧prebuilt DLL内のlibusb版を推測して対応sourceとすることはせず、今回の実buildを根拠にします。別PC/clean Windows/実USB抜去/屋外観測の未検証は、公開化だけでは解消しません。
