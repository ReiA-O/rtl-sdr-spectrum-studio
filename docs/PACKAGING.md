# Windows x64配布フォルダーの作成

これは開発PCで明示実行する手順です。受取側PCはZIPを展開して起動でき、Rust/Python/Node/Build Toolsを必要としません。作成側にはRust MSVCとMicrosoft C++ Build Tools、Windows PowerShellが必要です。自動USBドライバー導入・受信開始・機器設定変更は行いません。

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

## 2. パッケージ入力の取得

```powershell
$prepared = .\tools\prepare-packaging.ps1 | ConvertFrom-Json
```

このコマンドは公式URLから以下を取得し、記録したSHA256と照合します。既存ファイルの内容が異なる場合は上書きせず停止します。

- RTL-SDR Blog V1.4.0公式Release.zip。x64のrtlsdr.dllだけを`drivers/rtlsdr/`へ配置。
- 同タグのドライバーソースZIP、必要な原ライセンス文書。
- Windows x64の依存グラフを示すCargo metadata。これはローカルの開発パスを含むため`.gitignore`対象で、リポジトリへ追加しません。

ネットワークへ接続するのは、この明示的な準備工程です。受取側の通常の観測には不要です。Zadigや機器操作用exeは取得・実行しません。

## 3. 新規フォルダーとZIPを作成

```powershell
$binary = 'native\target-portable\release\rtl-spectrum-studio.exe'
$hash = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash
.\tools\package-studio.ps1 -BinaryPath $binary -ExpectedBinarySha256 $hash -PackageVersion 0.4.0-dev -CargoMetadataPath $prepared.cargo_metadata_path
```

Microsoftランタイムはインストール済みの正規`VC/Redist/MSVC/.../x64/Microsoft.VC*.CRT/vcruntime140.dll`から取得します。自動検出先と異なる場合は`-RuntimeDllPath`で明示します。Rust標準ライブラリーの通知を別の場所へ導入した場合は`-RustCopyrightPath`を指定できます。System32からDLLを抜き出さないでください。

同名成果物を置換せず、`dist/`へ時刻付きで新規作成します。元の`data/`は読まず、`presets/fm-measurement.json`を新しいsettingsの初期値へコピーします。これまでのデモ未同梱ZIPでは録音フォルダーは空です。新しい承認済みデモだけを同梱する拡張は次項で区別します。

v0.4.0の初期プリセットは中心DC補正ONです。中心付近の実信号にも影響し、処理中のレベルは相対表示になります。中心AM局は搬送波が抑えられて包絡復調が崩れる場合があるため、OFF比較か、受信中心を局からずらしてVFOで合わせる運用を使います。RF感度／S/N改善ではありません。原IQ保存は補正前です。補正OFFを初期値にする場合は、個人情報のない別のプリセットを用意して`-PresetPath`で明示します。パッケージ生成の成功と、[VALIDATION.md](VALIDATION.md) の製品受入れは別の確認です。

PE依存・x64・承認バイナリーSHA256・driver原ZIPとの一致・ZIP全ファイルhashを検査します。`-ForbiddenText`へ公開してはいけない文字列の配列を渡すと、全同梱ファイルをUTF8/UTF16LE/BEで検査してhit時に停止します。秘密文字列そのものをソースやmanifestへ保存しないでください。

ライセンス通知・DLLの対応ソース・元コードの公開用ライセンスは別々に確認します。特に公式driverに静的に含まれるlibusbの正確な版とビルド由来は未確認で、外部配布の包括的な適合性確認済みとは扱いません。

### AFG録音IQデモの同梱

明示承認されたAFG実録IQと対応プリセット・案内を`demo/`へ用意しています。`demo/manifest.json`の正確なファイル名・サイズ・SHA256で確認して同梱します。manifestのschemaは`spectrum-studio-distribution-demo-1`です。`-DemoManifestPath`でmanifestを指定でき、未指定時はリポジトリの`demo/manifest.json`が存在すれば使います。存在しない時に個人の録音フォルダーを探してコピーする動作はありません。

許可するのは`demo_afg_fm_32mhz.sigmf-meta`／`demo_afg_fm_32mhz.sigmf-data`、`AFG_FM_32MHz_demo.json`、デモ`README.md`の4ファイルです。IQ pairは配布先の`data/recordings/`直下、プリセットは`data/settings/presets/`、案内と監査用manifestは`demo/`へ配置します。相対パスの逸脱・未知の出力先・重複・サイズ／hash不一致は拒否し、元の`data/`全体は読み込みません。Gitも承認した2つのSigMF pathだけを例外とし、rawファイルへbinary属性を指定してbyte hashを保ちます。

この同梱拡張ではpackage契約67件・export10件、最終デモZIPの全entry hash・private文字列検査・自己診断が成功しました。受信DLLを無効化した模擬状態で最終ZIPの再生14項目も成功しています。実GUI9項目は案内追記前の候補で確認し、最終版とexe／IQ／metadata／presetは同一hashです。本体exeはv0.4.0と同じで、デモ同梱のためのコード再ビルドはしていません。

同梱後は「プリセット」から`AFG_FM_32MHz_demo`を読み込み、**設定画面右上の×で閉じてから**「保存データ」へ進みます。デモの「IQ再生」→「再生開始」後に音声をONにします。VFO32 MHz・WFM200 kHz、手動FFT4096点・実効RBW375 Hzの再生用設定と、従来の測定用初期設定は別のプリセットとして保持します。個人の通常受信記録・校正情報・機器IP／シリアル・ローカルパスは同梱対象にしません。

## 4. 別工程の確認

`--self-check`でルート検出、DLL読み込み、内蔵UIを確認できます。受信・サーバー・録音を開始しません。結果はパッケージ外へ保存し、完成ZIPのhashを保ちます。実画面・音声・別PC・屋外受信は、それぞれ別に確認してください。

デモの固定4ファイルとmanifestはバイト列で検査します。Git取得時もmanifestと一致するよう、IQはbinary、同梱metadata・preset・README・manifestは固定pathの-text属性で改行変換を抑止します。通常sourceの改行方針は維持します。
