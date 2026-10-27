[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$BinaryPath,
    [Parameter(Mandatory=$true)][ValidatePattern('^[A-Fa-f0-9]{64}$')][string]$ExpectedBinarySha256,
    [string]$PackageVersion = '0.4.0-dev',
    [string]$OutputParent,
    [string]$PresetPath,
    [string]$RuntimeDllPath,
    [string]$CargoMetadataPath,
    [string]$RustCopyrightPath,
    [string]$DemoManifestPath,
    [string[]]$ForbiddenText = @()
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$project = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))


# Only the four explicitly approved demo assets can enter a distribution.
function Read-ApprovedDemoBundle([string]$ManifestPath) {
    $manifestItem=Get-Item -LiteralPath $ManifestPath -ErrorAction Stop
    if ($manifestItem -isnot [IO.FileInfo] -or $manifestItem.Length -lt 1 -or $manifestItem.Length -gt 262144 -or ($manifestItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'Demo manifest must be a regular file of at most 256 KiB.' }
    $manifestPathResolved=$manifestItem.FullName
    $demoParent=[IO.Path]::GetDirectoryName($manifestPathResolved)
    $catalogText=[IO.File]::ReadAllText($manifestPathResolved)
    if (!$catalogText.TrimStart().StartsWith('{',[StringComparison]::Ordinal)) { throw 'Demo manifest root must be a JSON object.' }
    $catalog=$catalogText | ConvertFrom-Json
    if ($catalog -isnot [pscustomobject]) { throw 'Demo manifest must be an object.' }
    $catalogFields=@($catalog.PSObject.Properties.Name)
    if ($catalogFields.Count -ne 2 -or @($catalogFields | Where-Object { $_ -cnotin @('schema','files') }).Count -gt 0 -or $catalog.schema -isnot [string] -or $catalog.schema -cne 'spectrum-studio-distribution-demo-1') { throw 'Unsupported demo manifest fields or schema.' }
    if ($catalog.files -isnot [array] -or $catalog.files.Count -ne 4) { throw 'Demo manifest must contain exactly four approved files.' }
    $demoDestinations=@{
        'demo_afg_fm_32mhz.sigmf-data'='data/recordings/demo_afg_fm_32mhz.sigmf-data'
        'demo_afg_fm_32mhz.sigmf-meta'='data/recordings/demo_afg_fm_32mhz.sigmf-meta'
        'AFG_FM_32MHz_demo.json'='data/settings/presets/AFG_FM_32MHz_demo.json'
        'README.md'='demo/README.md'
    }
    $demoSeenSources=[Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $demoSeenDestinations=[Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $approvedFiles=@()
    foreach ($demoEntry in $catalog.files) {
        if ($demoEntry -isnot [pscustomobject]) { throw 'Demo file entry must be an object.' }
        $entryFields=@($demoEntry.PSObject.Properties.Name)
        if ($entryFields.Count -ne 4 -or @($entryFields | Where-Object { $_ -cnotin @('path','destination','bytes','sha256') }).Count -gt 0) { throw 'Unknown or missing demo file fields.' }
        if ($demoEntry.path -isnot [string] -or $demoEntry.path -cnotin @($demoDestinations.Keys)) { throw 'Demo source path is not approved.' }
        if ($demoEntry.destination -isnot [string] -or $demoEntry.destination -cne $demoDestinations[$demoEntry.path]) { throw 'Demo destination does not match the approved source.' }
        if (!$demoSeenSources.Add($demoEntry.path) -or !$demoSeenDestinations.Add($demoEntry.destination)) { throw 'Duplicate demo source or destination.' }
        if ($demoEntry.bytes -isnot [ValueType] -or $demoEntry.bytes -is [bool]) { throw 'Demo byte count must be a finite integer.' }
        $byteCount=[double]$demoEntry.bytes
        $maximum=if ($demoEntry.path -ceq 'demo_afg_fm_32mhz.sigmf-data') { 67108864 } else { 262144 }
        if ([double]::IsNaN($byteCount) -or [double]::IsInfinity($byteCount) -or $byteCount -lt 1 -or $byteCount -gt $maximum -or $byteCount -ne [Math]::Floor($byteCount)) { throw 'Demo file exceeds its size limit or has an invalid byte count.' }
        if ($demoEntry.path -ceq 'demo_afg_fm_32mhz.sigmf-data' -and ($byteCount -lt 2 -or $byteCount % 2 -ne 0)) { throw 'Demo IQ must contain complete interleaved I/Q byte pairs.' }
        if ($demoEntry.sha256 -isnot [string] -or $demoEntry.sha256 -notmatch '^[A-Fa-f0-9]{64}$') { throw 'Demo SHA256 is invalid.' }
        $sourcePath=[IO.Path]::GetFullPath((Join-Path $demoParent $demoEntry.path))
        if (!$sourcePath.StartsWith($demoParent+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)) { throw 'Demo source escapes the manifest directory.' }
        $sourceItem=Get-Item -LiteralPath $sourcePath -ErrorAction Stop
        if ($sourceItem -isnot [IO.FileInfo] -or ($sourceItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -or $sourceItem.Length -ne [long]$byteCount) { throw 'Demo source file type or size does not match.' }
        $sourceHash=(Get-FileHash -LiteralPath $sourcePath -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($sourceHash -cne $demoEntry.sha256.ToLowerInvariant()) { throw 'Demo source SHA256 does not match.' }
        $approvedFiles+=@{path=$demoEntry.path;source_path=$sourcePath;destination=$demoEntry.destination;bytes=[long]$byteCount;sha256=$sourceHash}
    }
    return [pscustomobject]@{manifest_path=$manifestPathResolved;manifest_sha256=(Get-FileHash -LiteralPath $manifestPathResolved -Algorithm SHA256).Hash.ToLowerInvariant();files=$approvedFiles}
}

$demoBundle=$null
if (!$DemoManifestPath) {
    $defaultDemoManifest=Join-Path $project 'demo\manifest.json'
    if (Test-Path -LiteralPath $defaultDemoManifest -PathType Leaf) { $DemoManifestPath=$defaultDemoManifest }
}
if ($DemoManifestPath) { $demoBundle=Read-ApprovedDemoBundle $DemoManifestPath }

if (!$OutputParent) { $OutputParent = Join-Path $project 'dist' }
if (!$PresetPath) { $PresetPath = Join-Path $project 'presets\fm-measurement.json' }
if (!$CargoMetadataPath) { $CargoMetadataPath = Join-Path $PSScriptRoot 'portable-support\cargo-metadata-windows.json' }
if (!$RustCopyrightPath) { $RustCopyrightPath = Join-Path $env:USERPROFILE '.rustup\toolchains\stable-x86_64-pc-windows-msvc\share\doc\rust' }
if (!$RuntimeDllPath) {
    $redistRoot = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\18\BuildTools\VC\Redist\MSVC'
    $candidates = @(Get-ChildItem -LiteralPath $redistRoot -Directory | Where-Object Name -Match '^\d+\.\d+\.\d+$' | Sort-Object { [version]$_.Name } -Descending)
    foreach ($candidate in $candidates) {
        $matches = @(Get-ChildItem -LiteralPath (Join-Path $candidate.FullName 'x64') -Filter vcruntime140.dll -File -Recurse -ErrorAction SilentlyContinue)
        if ($matches.Count) { $RuntimeDllPath = $matches[0].FullName; break }
    }
}
$support = Join-Path $PSScriptRoot 'portable-support'
$rtl = Join-Path $project 'drivers\rtlsdr\rtlsdr.dll'
$BinaryPath = (Resolve-Path -LiteralPath $BinaryPath).Path
$assetManifest=[IO.File]::ReadAllText((Join-Path $support 'provenance.json')) | ConvertFrom-Json
foreach ($asset in $assetManifest.assets) {
    $assetPath=[IO.Path]::GetFullPath((Join-Path $support $asset.path))
    if (!$assetPath.StartsWith($support+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)) { throw 'Invalid support asset path.' }
    if ((Get-FileHash -LiteralPath $assetPath -Algorithm SHA256).Hash -ne $asset.sha256) { throw "Support asset checksum mismatch: $($asset.path)" }
}
foreach ($required in @($BinaryPath,$PresetPath,$RuntimeDllPath,$rtl,(Join-Path $support 'licenses\rtl-sdr-COPYING-GPL-2.0.txt'),(Join-Path $support 'licenses\libusb-COPYING-LGPL-2.1.txt'),(Join-Path $support 'sources\rtl-sdr-blog-V1.4.0-source.zip'),(Join-Path $RustCopyrightPath 'COPYRIGHT-library.html'))) {
    if (!$required -or !(Test-Path -LiteralPath $required -PathType Leaf)) { throw "Required packaging input missing: $required" }
}
if ((Get-FileHash -LiteralPath $BinaryPath -Algorithm SHA256).Hash -ne $ExpectedBinarySha256) { throw 'The application binary does not match the approved build SHA256.' }
$driverArchive = Join-Path $project '.cache\driver\rtl-sdr-blog-V1.4.0-Release.zip'
if ((Get-FileHash -LiteralPath $driverArchive -Algorithm SHA256).Hash -ne '7EF33F1304647F65E5E0FDE43637A73D54F076E91E651A3CECC4F55A17FD9815') { throw 'Driver release archive SHA256 mismatch.' }
Add-Type -AssemblyName System.IO.Compression.FileSystem
$driverZip=[IO.Compression.ZipFile]::OpenRead($driverArchive)
try {
    $driverEntry=@($driverZip.Entries | Where-Object { $_.FullName -match '(^|/)x64/rtlsdr\.dll$' })
    if ($driverEntry.Count -ne 1) { throw 'Official archive x64 rtlsdr.dll entry not found uniquely.' }
    $driverStream=$driverEntry[0].Open();$driverSha=[Security.Cryptography.SHA256]::Create()
    try { $driverExpectedHash=[BitConverter]::ToString($driverSha.ComputeHash($driverStream)).Replace('-','') } finally { $driverSha.Dispose();$driverStream.Dispose() }
    if ((Get-FileHash -LiteralPath $rtl -Algorithm SHA256).Hash -ne $driverExpectedHash) { throw 'The driver DLL differs from the verified official release archive.' }
} finally { $driverZip.Dispose() }

function Get-PeInfo([string]$Path) {
    $bytes = [IO.File]::ReadAllBytes($Path)
    $pe = [BitConverter]::ToInt32($bytes,0x3c)
    if ([BitConverter]::ToUInt32($bytes,$pe) -ne 0x4550) { throw "Not a PE image: $Path" }
    $machine = [BitConverter]::ToUInt16($bytes,$pe+4)
    $sectionsCount = [BitConverter]::ToUInt16($bytes,$pe+6)
    $optionalSize = [BitConverter]::ToUInt16($bytes,$pe+20)
    $optional = $pe+24
    if ($machine -ne 0x8664 -or [BitConverter]::ToUInt16($bytes,$optional) -ne 0x20b) { throw "Not a Windows x64 PE image: $Path" }
    $sections = @()
    for ($i=0; $i -lt $sectionsCount; $i++) {
        $offset = $optional+$optionalSize+40*$i
        $sections += @{ VA=[BitConverter]::ToUInt32($bytes,$offset+12); Size=[Math]::Max([BitConverter]::ToUInt32($bytes,$offset+8),[BitConverter]::ToUInt32($bytes,$offset+16)); Raw=[BitConverter]::ToUInt32($bytes,$offset+20) }
    }
    function RvaOffset([uint32]$rva) {
        foreach ($section in $sections) { if ($rva -ge $section.VA -and $rva -lt $section.VA+$section.Size) { return [int]($rva-$section.VA+$section.Raw) } }
        throw "Invalid PE RVA $rva in $Path"
    }
    $imports = @()
    $importRva = [BitConverter]::ToUInt32($bytes,$optional+120)
    if ($importRva) {
        $offset = RvaOffset $importRva
        while ([BitConverter]::ToUInt32($bytes,$offset+12)) {
            $nameOffset = RvaOffset ([BitConverter]::ToUInt32($bytes,$offset+12))
            $end=$nameOffset; while ($bytes[$end]) { $end++ }
            $imports += [Text.Encoding]::ASCII.GetString($bytes,$nameOffset,$end-$nameOffset)
            $offset += 20
        }
    }
    return @{ machine='x86_64'; imports=@($imports | Sort-Object -Unique); file_version=[Diagnostics.FileVersionInfo]::GetVersionInfo($Path).FileVersion }
}
$peInfo = [ordered]@{ application=Get-PeInfo $BinaryPath; rtlsdr=Get-PeInfo $rtl; vcruntime=Get-PeInfo $RuntimeDllPath }
foreach ($image in $peInfo.Values) {
    foreach ($dependency in $image.imports) {
        if ($dependency -notmatch '^(?i:api-ms-win-.*|ext-ms-win-.*|kernel32|ntdll|bcryptprimitives|ws2_32|vcruntime140|ucrtbase|advapi32|user32|shell32|ole32|secur32|userenv)\.dll$') { throw "Unexpected DLL dependency requires review: $dependency" }
    }
}
$presetBytes=[IO.File]::ReadAllBytes($PresetPath)
$presetText=[Text.Encoding]::UTF8.GetString($presetBytes)
$preset=$presetText | ConvertFrom-Json
if ($preset.version -ne 1 -or !$preset.analysis -or !$preset.input -or !$preset.vfos) { throw 'Preset does not have the application configuration schema.' }
if ($presetText -match '(?i)[A-Z]:[\\/]|\\\\[?\\]') { throw 'Preset contains a machine-specific absolute path.' }
if ($PackageVersion -notmatch '^[A-Za-z0-9._-]{1,40}$') { throw 'Invalid package version.' }
$stamp=Get-Date -Format 'yyyyMMdd-HHmmss-fff'
$name="RTL-SDR-Spectrum-Studio-$PackageVersion-windows-x64-$stamp"
[IO.Directory]::CreateDirectory([IO.Path]::GetFullPath($OutputParent)) | Out-Null
$destination=Join-Path ([IO.Path]::GetFullPath($OutputParent)) $name
$zipPath="$destination.zip"
if ((Test-Path -LiteralPath $destination) -or (Test-Path -LiteralPath $zipPath)) { throw 'Refusing to replace an existing package.' }
New-Item -ItemType Directory -Path $destination | Out-Null
function Write-New([string]$Relative,[string]$Text,[bool]$Ascii=$false) {
    $target=Join-Path $destination $Relative
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($target)) | Out-Null
    $encoding=if ($Ascii) { [Text.Encoding]::ASCII } else { New-Object Text.UTF8Encoding($false) }
    $stream=[IO.File]::Open($target,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write)
    try { $out=$encoding.GetBytes($Text);$stream.Write($out,0,$out.Length) } finally { $stream.Dispose() }
}
function Copy-New([string]$Source,[string]$Relative) {
    $target=Join-Path $destination $Relative
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($target)) | Out-Null
    [IO.File]::Copy($Source,$target,$false)
}
Copy-New $BinaryPath 'rtl-spectrum-studio.exe'
Copy-New $RuntimeDllPath 'vcruntime140.dll'
Copy-New $rtl 'drivers\rtlsdr\rtlsdr.dll'
$presetName=[IO.Path]::GetFileName($PresetPath)
Copy-New $PresetPath (Join-Path 'data\settings\presets' $presetName)
Copy-New $PresetPath 'data\settings\last-session.json'
[IO.Directory]::CreateDirectory((Join-Path $destination 'data\recordings')) | Out-Null

$approvedDemoFiles=@()
if ($demoBundle) {
    foreach ($approvedDemoFile in $demoBundle.files) {
        Copy-New $approvedDemoFile.source_path $approvedDemoFile.destination
        $copiedDemoPath=Join-Path $destination $approvedDemoFile.destination
        $copiedDemoHash=(Get-FileHash -LiteralPath $copiedDemoPath -Algorithm SHA256).Hash.ToLowerInvariant()
        if ((Get-Item -LiteralPath $copiedDemoPath).Length -ne $approvedDemoFile.bytes -or $copiedDemoHash -cne $approvedDemoFile.sha256) { throw 'Copied demo bytes differ from the approved manifest.' }
        $approvedDemoFiles+=@{path=$approvedDemoFile.path;destination=$approvedDemoFile.destination;bytes=$approvedDemoFile.bytes;sha256=$copiedDemoHash}
    }
    Copy-New $demoBundle.manifest_path 'demo/manifest.json'
    if ((Get-FileHash -LiteralPath (Join-Path $destination 'demo/manifest.json') -Algorithm SHA256).Hash.ToLowerInvariant() -cne $demoBundle.manifest_sha256) { throw 'Copied demo manifest changed during packaging.' }
}

Write-New 'studio-root.json' ('{"app_id":"rtl-sdr-spectrum-studio","layout_version":1,"distribution":"private-portable-development","package_version":"'+$PackageVersion+'"}'+"`n")
$launcher=@'
@echo off
setlocal
cd /d "%~dp0"
set "STUDIO_RTLSDR_DLL=%~dp0drivers\rtlsdr\rtlsdr.dll"
"%~dp0rtl-spectrum-studio.exe" --project-dir "%~dp0." %*
set "STUDIO_EXIT=%ERRORLEVEL%"
if not "%STUDIO_EXIT%"=="0" pause
exit /b %STUDIO_EXIT%
'@
Write-New 'start-studio.cmd' (($launcher -replace "`r?`n","`r`n")+"`r`n") $true
$guide=@'
# RTL-SDR Spectrum Studio - Windows portable development build

1. ZIPをすべて書込み可能なフォルダーへ展開する。ZIP内から直接起動しない。
2. `start-studio.cmd` を開く。ローカルブラウザー画面が開く。
3. 実機で受信する場合は受信機・周波数・ゲインを確認してから、画面の受信開始を押す。同梱デモの再生は下記の手順を使う。
4. 終了前に録音と受信を停止し、起動したコンソールで Ctrl+C を押す。

Windows 10/11 x64 と通常のブラウザーを使用する。Rust/Python/Node/Build Toolsの追加は不要。
設定・プリセットは `data/settings/`、新しいIQ/WAVは `data/recordings/` に保存する。
起動時に受信・Arm・IQ/WAV録音を自動開始しない。実機、アンテナ、受信環境を選んで使用する。
測定用プリセットを初期値に同梱。32.256 MHz / 2.048 MS/s / RF gain 20.7 dB、Span 900 kHz、Ref -55 dBFS、Range 30 dB は一つの実験条件で、屋外受信の万能設定ではない。実効RBW／ENBWは画面で確認する。

持出先PCでは RTL-SDR Interface 0 用 WinUSB ドライバーが別途必要。USBドライバーの導入・変更はこのアプリでは自動実行しない。
公式手順: https://www.rtl-sdr.com/rtl-sdr-quick-start-guide/
アプリは127.0.0.1だけで待受ける。通常の受信・表示・録音はネット接続不要。
ポート8765が使用中の場合は `start-studio.cmd --port 8766` を使用できる。
画面を開かない診断: `start-studio.cmd --self-check`（受信・サーバー・録音は開始しない）。

新しい版は別フォルダーへ展開する。既存版の上書き、dataフォルダーの削除、既存録音の移動は行わない。
このZIPには明示承認したAFGデモ以外の元PCのIQ/WAV、履歴、校正情報、個人ファイルを含めない。元のユーザーデータは変更しない。
再生・スピーカー・持出先PCの実機動作は、それぞれのPCでの確認が必要。
Spectrumの「ピークリセット」でMax／Min／Averageの履歴を同時にリセットし、現在のLiveから再集計する。受信・録音は継続し、入力条件・密度・Waterfall・Marker位置は維持する。
Densityの上限・下限は同パネルで手動設定できる。新しい範囲では密度だけを再集計し、Spectrumの保持値やWaterfall・受信は維持する。
VFOの「受信・音声補助」でAFC、RFスケルチ、音声ゲイン、ピーク保護、クリック抑制、WAV経路を設定する。AFC／スケルチは初期OFF。AFCをOFFに戻すと補正0になり、名目局周波数で受信する。
WAVの既定は「復調音（後処理なし）」で、AFC／帯域フィルターは適用される。「聴取処理後」を選ぶとスケルチ／音声ゲイン／保護を含めて保存する。条件変更中のWAVは分割し、経路と処理統計を記録する。
音声補助とAFC自体は原IQやSpectrum／測定を変更しない。音声ピーク保護はsample振幅の保護で、true-peak規格の保証ではない。
v0.4.0の中心DC補正は既定ON、手動OFF。公称20 Hz一極追従減算を表示・ピーク・測定・トリガ・復調の共通経路へ適用する。中心の実信号にも影響し、適用中は絶対レベル校正表示を使えない。原IQは未補正で保存し、WAVには復調経路の補正が反映される。
とくに中心AM局は実搬送波が抑えられ、包絡復調が崩れる場合がある。OFF比較か、受信中心を局からずらしてVFOで合わせる運用を使う。RF感度やS/Nを改善する機能ではない。
Arm・IQ保存・WAV録音中は中心DC補正の切替を拒否する。切替時は保持Trace・密度・Waterfall・音声を初期化する。Direct Samplingは適用外。保存IQの再生開始・シークでは補正状態を再初期化する。
自動／ユーザーマーカーのピークから選択VFOへ同調できる。受信中心は自動で動かさず、有効帯域外や未適用のVFO編集がある状態では同調を保留する。
v0.4.0は同じ開発PCで設定境界・保存IQ API・50Ω終端の有限受信・DEMO実GUI・ZIP／自己診断を確認済み。各版の条件はリポジトリのdocs/VALIDATION.mdを参照する。実RF全域・感度／SFDR／alias／AGC実応答、クリーンな別PC、屋外ノートPC、長時間運転・聴取品質は未検証。このZIPはdev版です。

`package-manifest.json` と `package-validation.json` に同梱物と検査結果を保存する。
本体の画面はexeに内蔵し、外部サーバーからUI資産を取得しない。
これは個人用の開発版。第三者への公開・配布用のライセンス選定や完全なクリーンPC受入れを済ませた意味ではない。
'@

if ($demoBundle) {
    $guide+=@'


## 実機なしで同梱AFGデモを再生する

この版には、AFGで作ったFM信号をRTL-SDRで実収録した承認済みデモだけを同梱する。放送・会話などの個人収録データは含めない。受信機を接続せずに再生できる。
「プリセット」→「AFG_FM_32MHz_demo」を読み込み、設定画面の×で閉じる→「保存データ」→「demo_afg_fm_32mhz」の「IQ再生」→上部「再生開始」→「音声ON」の順で操作する。
対象は搬送波32 MHzのFM。デモ用VFOの周波数・帯域を使い、受信中心に局を置かない。元のIQは未補正であり、中心DC補正ON/OFFを比較できる。再生位置はIQ再生パネルのスライダーで変更できる。
同梱データの由来・条件は `demo/README.md`、データと設定のhashは `demo/manifest.json` に記録する。再生開始・音声出力は手動で行い、実機受信を自動開始しない。
'@
} else {
    $guide+="`n`nこのパッケージには保存IQのデモファイルを同梱していない。保存IQの再生は、アプリ内へSigMFのmetadataとdataを置き『保存データ』から開始する。`n"
}

Write-New 'はじめに.md' $guide
foreach ($item in Get-ChildItem -LiteralPath (Join-Path $support 'licenses') -File) { Copy-New $item.FullName (Join-Path 'licenses' $item.Name) }
foreach ($item in Get-ChildItem -LiteralPath (Join-Path $support 'sources') -File) { Copy-New $item.FullName (Join-Path 'third-party-source' $item.Name) }
Copy-New (Join-Path $RustCopyrightPath 'COPYRIGHT-library.html') 'licenses\rust-standard-library\COPYRIGHT-library.html'
foreach ($item in Get-ChildItem -LiteralPath (Join-Path $RustCopyrightPath 'licenses') -File) { Copy-New $item.FullName (Join-Path 'licenses\rust-standard-library\licenses' $item.Name) }
$crateNotices=@();$metadata=[IO.File]::ReadAllText($CargoMetadataPath) | ConvertFrom-Json
$nodes=@{};foreach ($node in $metadata.resolve.nodes) { $nodes[$node.id]=$node }
$reachable=[Collections.Generic.HashSet[string]]::new();$queue=[Collections.Generic.Queue[string]]::new();$queue.Enqueue($metadata.resolve.root)
while ($queue.Count) { $id=$queue.Dequeue();if ($reachable.Add($id)) { foreach ($dependency in $nodes[$id].deps) { $queue.Enqueue($dependency.pkg) } } }
foreach ($crate in $metadata.packages | Where-Object { $_.source -and $reachable.Contains($_.id) }) {
    $crateName=$crate.name;$crateVersion=$crate.version;$crateId="$crateName-$crateVersion"
    $crateDir=[IO.Path]::GetDirectoryName($crate.manifest_path)
    if (!(Test-Path -LiteralPath $crateDir -PathType Container)) { throw "Resolved Windows dependency missing: $crateId" }
    $declared=$crate.license
    $copied=@()
    $files=@(Get-ChildItem -LiteralPath $crateDir -File | Where-Object Name -Match '(?i)^(LICENSE|COPYING|NOTICE|COPYRIGHT)')
    foreach ($sub in Get-ChildItem -LiteralPath $crateDir -Directory | Where-Object Name -Match '(?i)^licenses?$') { $files+=@(Get-ChildItem -LiteralPath $sub.FullName -File -Recurse) }
    foreach ($file in $files) { $rel=$file.FullName.Substring($crateDir.Length+1);Copy-New $file.FullName (Join-Path (Join-Path 'licenses\rust-crates' $crateId) $rel);$copied+=$rel.Replace('\','/') }
    if (!$copied.Count) { throw "No license notice found for cached crate $crateId" }
    $crateNotices+=@{ name=$crateName;version=$crateVersion;license_expression=$declared;notices=$copied }
}
Write-New 'licenses\rust-crates.json' (@{ resolved_windows_crates=$crateNotices;scope='Transitive reachable dependency graph from cargo metadata --offline --locked --filter-platform x86_64-pc-windows-msvc, including build/proc-macro dependencies.' } | ConvertTo-Json -Depth 12)
Copy-New (Join-Path $support 'NOTICE.md') 'licenses\THIRD-PARTY-NOTICES.md'
Copy-New (Join-Path $support 'provenance.json') 'licenses\upstream-provenance.json'
$files=@(Get-ChildItem -LiteralPath $destination -File -Recurse | ForEach-Object { @{ path=$_.FullName.Substring($destination.Length+1).Replace('\','/');bytes=$_.Length;sha256=(Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant() } })
$manifest=@{ application='RTL-SDR Spectrum Studio';package_version=$PackageVersion;created_at=(Get-Date).ToString('o');architecture='windows-x64';web_assets='embedded in executable';startup_receive=$false;startup_arm=$false;startup_record=$false;user_data='New package-local data; approved AFG demo only when included; no private user recordings/session copied';approved_demo_included=($null -ne $demoBundle);approved_demo_manifest_sha256=$(if ($demoBundle) { $demoBundle.manifest_sha256 } else { $null });approved_demo_files=$approvedDemoFiles;initial_preset=$presetName;initial_preset_sha256=(Get-FileHash -LiteralPath $PresetPath -Algorithm SHA256).Hash.ToLowerInvariant();pe_images=$peInfo;files=$files }
Write-New 'package-manifest.json' ($manifest | ConvertTo-Json -Depth 14)
$validation=@{ pe_x64_and_dependencies_checked=$true;approved_binary_hash_matches=$true;no_absolute_path_in_initial_preset=$true;privacy_utf8_utf16_all_files_checked=($ForbiddenText.Count -gt 0);privacy_term_count=$ForbiddenText.Count;existing_user_data_modified=$false;private_user_recordings_copied=$false;approved_demo_included=($null -ne $demoBundle);approved_demo_files=$approvedDemoFiles;application_started_by_packager=$false;hardware_operated_by_packager=$false;self_check_executed=$false;clean_windows_pc_tested=$false;outside_laptop_rf_tested=$false }
Write-New 'package-validation.json' ($validation | ConvertTo-Json -Depth 8)
$privacyLatin1=[Text.Encoding]::GetEncoding(28591)
foreach ($privacyFile in Get-ChildItem -LiteralPath $destination -File -Recurse) {
    $privacyBytes=[IO.File]::ReadAllBytes($privacyFile.FullName)
    $privacyRaw=$privacyLatin1.GetString($privacyBytes)
    foreach ($privacyEncoding in @([Text.Encoding]::UTF8,[Text.Encoding]::Unicode,[Text.Encoding]::BigEndianUnicode)) {
        $privacyDecoded=$privacyEncoding.GetString($privacyBytes)
        foreach ($privacyTerm in $ForbiddenText) {
            if (!$privacyTerm) { continue }
            $privacyNeedle=$privacyLatin1.GetString($privacyEncoding.GetBytes($privacyTerm))
            if ($privacyDecoded.IndexOf($privacyTerm,[StringComparison]::OrdinalIgnoreCase) -ge 0 -or $privacyRaw.IndexOf($privacyNeedle,[StringComparison]::OrdinalIgnoreCase) -ge 0) {
                throw ('Private text found in package file: '+$privacyFile.FullName.Substring($destination.Length+1))
            }
        }
    }
}
Add-Type -AssemblyName System.IO.Compression.FileSystem
[IO.Compression.ZipFile]::CreateFromDirectory($destination,$zipPath,[IO.Compression.CompressionLevel]::Optimal,$true)
$archive=[IO.Compression.ZipFile]::OpenRead($zipPath)
try {
    foreach ($entry in $archive.Entries) {
        if (!$entry.Name) { continue }
        $relative=$entry.FullName.Substring($name.Length+1).Replace('/',[IO.Path]::DirectorySeparatorChar)
        $expected=(Get-FileHash -LiteralPath (Join-Path $destination $relative) -Algorithm SHA256).Hash
        $stream=$entry.Open();$sha=[Security.Cryptography.SHA256]::Create()
        try { $actual=[BitConverter]::ToString($sha.ComputeHash($stream)).Replace('-','') } finally { $sha.Dispose();$stream.Dispose() }
        if ($actual -ne $expected) { throw "ZIP verification failed: $relative" }
    }
} finally { $archive.Dispose() }
$result=@{ package_folder=$destination;zip_path=$zipPath;zip_sha256=(Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash.ToLowerInvariant();zip_bytes=(Get-Item -LiteralPath $zipPath).Length;zip_all_entry_hashes_verified=$true;application_started=$false }
$result | ConvertTo-Json -Depth 5
