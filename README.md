# RTL-SDR Spectrum Studio

RTL-SDRでAM/FM放送のIQ取得・音声復調・録音と、受信帯域の観測を行うWindows向け開発版です。Rust本体がローカルHTTPサーバーを動かし、exeに内蔵した画面を通常のブラウザーへ表示します。Pythonは数値比較・単体テスト用の参照実装で、アプリの実行には使いません。

配布対象の最新デモ同梱ZIPは **`RTL-SDR-Spectrum-Studio-0.4.0-dev-demo-windows-x64-20261027-210450-328.zip`**（11,296,451 bytes、SHA256 `d04cde25fc4e250837dd80ff4a5d5c19838f40261dd5906428aba0ec967892de`）です。本体exeはv0.4.0と同一で、IQデモ・専用プリセット・案内を加えた配布構成です。[Releases](https://github.com/ReiA-O/rtl-sdr-spectrum-studio/releases) の`v0.4.0-demo`へprivateの下書きとして保存済みで、GitHub asset digestも上記SHA256と一致しています。公開化はしていません。

v0.4.0は、帯域を決めてピークを選び、VFOで復調し、対象局を含む原IQを保存する導線を中心にしています。受信用の中心DC補正を既定ONで追加しました。最終配布物で設定境界・API・50Ω終端の受信・DEMOの画面操作を確認しました。条件と未検証部分は [VALIDATION.md](docs/VALIDATION.md) に記載しています。

現在はレビュー用のソース一式です。公開・販売向けのライセンス選定や製品全体の受入れ完了を意味しません。[LICENSE.md](LICENSE.md) と [検証範囲](docs/VALIDATION.md) を先に確認してください。

## 録音IQデモ

AFG3102から32 MHz・FM偏移±75 kHz・1 kHzの試験音を出力し、Studioで収録したIQを同梱しました。放送番組やマイク音声ではなく、測定器で作った既知信号の実RF収録です。8.32秒、Fs1.024 MS/s、受信中心32.256 MHz、原IQ 17,039,360 bytes。欠落とADC端値到達は0、収録音の1 kHz主成分とPCM端値到達0を数値確認しています。公開用metadataは必要項目から構成し、原IQのバイト列を保持しました。[デモ案内](demo/README.md) に条件と手順があります。

1. ZIPを展開し、`start-studio.cmd`を起動します。
2. 受信停止中に「プリセット」から **`AFG_FM_32MHz_demo`** を読み込み、**設定画面右上の×で閉じます**。この段階では受信開始を押しません。
3. 「保存データ」から **`demo_afg_fm_32mhz.sigmf-meta`** の「IQ再生」を選び、上部の **「再生開始」** を押します。
4. 音量を小さくし、「音声OFF」を押してONにすると試験音を出力します。終端で自動停止し、再生位置スライダーを先頭へ戻すと繰り返せます。「ライブへ戻る」で終了します。

保存IQ再生はRTL-SDR・AFG・WinUSBを使わない経路です。最終ZIPでは受信DLLを無効化した模擬状態で再生14項目を確認しました。実USB抜去や清浄な別PCでの受入れ、実際のスピーカー聴感を確認した意味ではありません。実GUI9項目は先行デモ候補で確認し、最終版では案内の×操作を追記しただけでexe・IQ・metadata・プリセットの4点は同一hashです。

FMでは最大のスペクトルピークが搬送中心とは限らないため、最初は同梱プリセットのVFO **32.000 MHz・WFM・200 kHz** で再生してください。手動FFT4096点、実効RBW375 Hzの設定です。既存の測定用初期プリセットは置き換えません。IQは`.sigmf-meta`と`.sigmf-data`をセットで扱い、復調音のWAVと区別します。

## 主な機能

- Spectrum・密度表示・Waterfall・複数VFOを同時表示。共通FFTから全有効フレームを集計します。
- AM/NFM/WFMモノラル復調、選択VFOだけのスピーカー出力、VFO別WAV録音。
- 中心DC補正は手動ON/OFF。独立I/Qの公称20 Hz一極追従減算を、表示・ピーク検索・測定・トリガ・復調の共通経路へ適用します。原IQは未補正のまま保存します。
- 自動／ユーザーマーカーのピークを選んで、選択VFOをその周波数へ同調できます。受信中心は自動変更せず、有効帯域外の同調を拒否します。
- VFOごとの任意AFC（初期OFF）、RFスケルチ、音声ゲイン・sampleピーク保護・既知不連続のクリック抑制。AFCは手動でOFFへ戻せ、名目局周波数と受信中心を保持します。
- WAVは復調音（音声補助前、既定）／聴取処理後を選択。中心DC補正・AFC・同調フィルターは両方へ反映し、スケルチ・音声ゲイン等は処理後経路へ反映します。
- 中心周波数・Span・RBW/FFT・Ref Level・Rangeの調整、TraceとPersistence、測定用プリセット。
- Spectrumの「ピークリセット」でMax／Min／Averageを同時にリセットし、受信や録音を続けたまま現在のLiveから再集計。
- Densityの上限・下限を手動設定。Spectrumの表示範囲から独立し、密度履歴だけを新範囲で再集計。
- 自動ピークマーカーとユーザーマーカー、Channel Power、測定帯域内のOBW。
- 生IQの手動保存、レベルトリガ、秒単位のプリ／ポストトリガ、保存IQの再生。

「中心DC補正」は受信中心の実信号にも影響します。とくにAM局を中心に置くと実際の搬送波まで抑えて包絡復調が崩れる場合があります。OFFで比較するか、受信中心を局の周波数からずらし、VFOを局へ合わせてください。RF感度やS/Nを改善する機能ではありません。Arm・IQ保存・WAV録音中の切替は拒否します。補正適用中のレベルは相対表示で、絶対レベル校正表示を無効化します。[処理式・再利用API](docs/FRONTEND.md) に実装と制約をまとめています。

VFOの「受信・音声補助」を開き、AFC等を変更して「補助に適用」を押します。AFCのチェックを外して再適用すると、補正量を0へ戻して名目周波数で受信します。AFC自体は測定用PSDと原IQを変更しません。中心DC補正とは独立した機能です。

詳しい動作と制約は [SPEC.md](docs/SPEC.md) にあります。密度表示はソフトウェアのFFT集計であり、専用測定器の捕捉率・測定精度と同等という意味ではありません。

継続確認が必要な点は [KNOWN_LIMITS.md](docs/KNOWN_LIMITS.md) に整理しています。

## 実行・ビルド

配布用ZIPを使う場合は、Windows 10/11 x64と通常のブラウザーを用意し、ZIPをすべて書込み可能な場所へ展開して`start-studio.cmd`を開きます。Rust・Python・Node・Build Toolsの追加は不要です。別PCでLIVE受信する場合は受信機のInterface 0用WinUSBドライバー設定が必要です。ドライバーの導入や受信開始は自動実行しません。

ソースからビルドするPCには、RustのMSVCツールチェーンとMicrosoft C++ Build Toolsが必要です。

```powershell
.\build-studio.cmd
.\start-studio.cmd
```

受信DLLがなくても起動でき、画面には未接続状態が表示されます。LIVE受信に使う公式ドライバーの取得とパッケージ生成は [PACKAGING.md](docs/PACKAGING.md) を参照してください。DLL取得は明示実行で行い、USBドライバーのインストールや機器設定変更は行いません。

起動時は受信・Arm・録音を停止しています。設定は`data/settings/`、新しいIQ/WAVは`data/recordings/`へ保存します。初回に同梱プリセットを読み込む場合は、受信停止中に`presets/fm-measurement.json`を設定プリセットとして配置するか、パッケージ生成時に初期値として同梱します。含まれる周波数やゲインは一つの実験条件で、屋外受信の万能設定ではありません。

```powershell
# サーバー・受信・録音を開始しない読み込み診断
native\target\release\rtl-spectrum-studio.exe --self-check --project-dir .
# ポートを変える場合
.\start-studio.cmd --port 8766
```

通常は127.0.0.1だけで待受けます。外部CDN等を使わず、ネット接続を必要としない構成です。持出先の実機環境での確認は未実施です。終了前に受信と録音を止め、起動したコンソールでCtrl+Cを押してください。

## テスト

```powershell
cargo test --manifest-path native\Cargo.toml --all-targets
node tests\ui-static-boundaries.cjs
node tests\ui-reception-boundaries.cjs
node --test tests\frontend-ui.test.cjs
# Python参照テストを使う場合だけNumPyが必要
python -m unittest discover -s tests -v
```

v0.4.0はRust108件・JavaScript66件が合格しました。JavaScriptはブラウザー・ネットワーク・実機を使わない状態遷移テストで、中心補正の表示、録音中のロック、旧frame棄却、ピーク選局も含みます。保存実機IQを明示指定する追加テスト1件は通常の`cargo test`ではignoredになり、個人の録音データは同梱しません。数値境界と製品API・実機・GUIの結果は [VALIDATION.md](docs/VALIDATION.md) で区別します。

再現用の数値probeは次のように実行します。`<新規出力フォルダー>`は毎回異なる名前へ置き換え、Git対象外の`verification/`または`data/`内を使ってください。JSONに加えてPCM検査用の小さなWAVも作成します。同名JSONは上書きせず失敗するため、既存の結果がある場所へ再実行しないでください。DLL・サーバー・実機を操作しない合成IQの検査です。

```powershell
cargo run --release --manifest-path native/Cargo.toml --example electrical_boundaries -- <新規出力フォルダー>/result.json
```

PowerShellでは、例えば出力先を`verification/electrical-probe-001/result.json`のような新規名に置き換えて実行します。

## 構成

- `native/`: Rustの受信・解析・録音・設定・HTTP本体とテスト。
- `native/examples/electrical_boundaries.rs`: 合成IQによるDSP数値境界の再現用probe。結果JSONとPCM検査用WAVはソースへ追加しません。
- `web/`: exeへ内蔵する画面。
- `studio/`, `tests/`: 数値参照用Pythonと単体テスト、JavaScriptの状態境界テスト。
- `presets/`: 個人情報を含まない測定用プリセット。
- `demo/`: 明示承認されたAFG実録IQ、再生プリセット、同梱manifestと案内。個人の通常受信記録は含めません。
- `tools/`: 明示実行のDLL取得・パッケージ生成。機器操作用の実験スクリプトは含めません。

明示承認されたAFGデモIQだけを例外対象とし、個人の録音・個人設定・校正情報・機器IP／シリアル番号・開発環境の絶対パス・非公開の実験記録はリポジトリに含めません。更新版は別フォルダーへ展開し、既存の`data/`を上書き・削除しないでください。
