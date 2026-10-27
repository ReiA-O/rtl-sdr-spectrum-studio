# Windows x64配布フォルダーの作成

これは開発PCで明示実行する手順です。受取側PCはZIPを展開して起動でき、Rust/Python/Node/Build Toolsを必要としません。作成側にはRust MSVCとMicrosoft C++ Build Tools、Windows PowerShellが必要です。自動USBドライバー導入・受信開始・機器設定変更は行いません。

## 1. テストとビルド

```powershell
cargo test --manifest-path native\Cargo.toml --all-targets
node tests\ui-static-boundaries.cjs
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
.\tools\package-studio.ps1 -BinaryPath $binary -ExpectedBinarySha256 $hash -PackageVersion 0.3.0-dev -CargoMetadataPath $prepared.cargo_metadata_path
```

Microsoftランタイムはインストール済みの正規`VC/Redist/MSVC/.../x64/Microsoft.VC*.CRT/vcruntime140.dll`から取得します。自動検出先と異なる場合は`-RuntimeDllPath`で明示します。Rust標準ライブラリーの通知を別の場所へ導入した場合は`-RustCopyrightPath`を指定できます。System32からDLLを抜き出さないでください。

同名成果物を置換せず、`dist/`へ時刻付きで新規作成します。元の`data/`は読まず、`presets/fm-measurement.json`だけを新しいsettingsの初期値へコピーします。録音フォルダーは空です。

PE依存・x64・承認バイナリーSHA256・driver原ZIPとの一致・ZIP全ファイルhashを検査します。`-ForbiddenText`へ公開してはいけない文字列の配列を渡すと、全同梱ファイルをUTF8/UTF16LE/BEで検査してhit時に停止します。秘密文字列そのものをソースやmanifestへ保存しないでください。

ライセンス通知・DLLの対応ソース・元コードの公開用ライセンスは別々に確認します。特に公式driverに静的に含まれるlibusbの正確な版とビルド由来は未確認で、外部配布の包括的な適合性確認済みとは扱いません。

## 4. 別工程の確認

`--self-check`でルート検出、DLL読み込み、内蔵UIを確認できます。受信・サーバー・録音を開始しません。結果はパッケージ外へ保存し、完成ZIPのhashを保ちます。実画面・音声・別PC・屋外受信は、それぞれ別に確認してください。
