# RTL-SDR Spectrum Studio

RTL-SDRでAM/FM放送のIQ取得・音声復調・録音と、受信帯域の観測を行うWindows向け開発版です。Rust本体がローカルHTTPサーバーを動かし、exeに内蔵した画面を通常のブラウザーへ表示します。Pythonは数値比較・単体テスト用の参照実装で、アプリの実行には使いません。

**公開プレビュー `v0.4.1-preview.1`** の取得先は [Releases](https://github.com/ReiA-O/rtl-sdr-spectrum-studio/releases/tag/v0.4.1-preview.1) です。Windows x64用のデモ同梱ZIPと、アプリ・全66 Rust crate・RTL-SDR/libusbの固定ソース・patch・ビルド手順を収めた対応ソースZIPを一緒に配布します。パッケージ版は **`0.4.1-public-preview.1`**、アプリ内部の版は **`0.4.1`** です。取得するassetの正確なファイル名とSHA256はリリースの案内・同梱manifestで確認してください。

v0.4.1は、帯域を決めてピークを選び、VFOで復調し、原IQを保存する受信経路を維持し、音声操作・終了操作・周波数表示を更新する版です。中心DC補正式、原IQの保存方式、ドライバーと同梱デモIQはv0.4.0から変更していません。旧版の数値検証はその条件での結果として保持し、新しい画面・配布実行物・RF gain反映の確認とは分けます。結果と未検証部分は [VALIDATION.md](docs/VALIDATION.md) に記載します。

オリジナルコード・文書・同梱デモには **GPL-3.0-or-later** を適用します。第三者のコードは各原ライセンスを維持します。[LICENSE.md](LICENSE.md)・[NOTICE.md](NOTICE.md) と [検証範囲](docs/VALIDATION.md) を確認してください。これは開発中のpre-releaseで、製品全体の受入れ、別PCや屋外での動作、RFの絶対レベル校正を保証するものではありません。

初回起動前に [Microsoft公式Visual C++ v14ランタイム（x64）](https://aka.ms/vc14/vc_redist.x64.exe) を導入してください。公開ZIPには`vcruntime140.dll`を同梱しません。

## 録音IQデモ

AFG3102から32 MHz・FM偏移±75 kHz・1 kHzの試験音を出力し、Studioで収録したIQを同梱しました。放送番組やマイク音声ではなく、測定器で作った既知信号の実RF収録です。8.32秒、Fs1.024 MS/s、受信中心32.256 MHz、原IQ 17,039,360 bytes。欠落とADC端値到達は0、収録音の1 kHz主成分とPCM端値到達0を数値確認しています。公開用metadataは必要項目から構成し、原IQのバイト列を保持しました。[デモ案内](demo/README.md) に条件と手順があります。

1. ZIPを展開し、`start-studio.cmd`を起動します。
2. 受信停止中に「プリセット」から **`AFG_FM_32MHz_demo`** を読み込み、**設定画面右上の×で閉じます**。この段階では受信開始を押しません。
3. 「保存データ」から **`demo_afg_fm_32mhz.sigmf-meta`** の「IQ再生」を選び、上部の **「再生開始」** を押します。
4. 音量を小さくし、「現在：音声OFF」を確認して「音声をONにする」を押すと試験音を出力します。終端で自動停止し、再生位置スライダーを先頭へ戻すと繰り返せます。「ライブへ戻る」で終了します。

保存IQ再生はRTL-SDR・AFG・WinUSBを使わない経路です。旧デモZIPでは受信DLLを無効化した模擬状態で再生14項目を確認しました。実USB抜去や清浄な別PCでの受入れ、実際のスピーカー聴感を確認した意味ではありません。実GUI9項目は先行デモ候補で確認し、当時の最終デモ版では案内の×操作を追記しただけでexe・IQ・metadata・プリセットの4点は同一hashです。

FMでは最大のスペクトルピークが搬送中心とは限らないため、最初は同梱プリセットのVFO **32.000 MHz・WFM・200 kHz** で再生してください。手動FFT4096点、実効RBW375 Hzの設定です。既存の測定用初期プリセットは置き換えません。IQは`.sigmf-meta`と`.sigmf-data`をセットで扱い、復調音のWAVと区別します。

## v0.4.1の画面操作

- 音声は「現在：音声ON/OFF」と、次に行う「音声をON/OFFにする」を別に表示します。開始処理が成功するまでONとは表示しません。ON設定でも受信停止・接続待ち・出力対象なしの場合は、その理由を補足表示します。
- ヘッダーの「アプリ終了」で確認画面を承認し、受信・再生と録音の終了処理を行います。「終了しました。ブラウザーのタブを閉じてください。」を確認してからタブを閉じます。未適用の編集値は適用されません。タブを閉じるだけではサーバーは終了しません。
- 周波数目盛り部分と周波数範囲表示の左右ドラッグ・同じ軸の左右キーは、受信中心だけを変更します。SpanとVFOの絶対周波数は固定し、帯域外になるVFOはダイアログを出さず自動停止します。この軸操作による停止は事前承認済みの操作として扱います。一般の手入力で中心を変える場合は、従来のD24の影響確認を継続します。ドラッグ中はプレビューで、指を離したときに適用します。録音・Arm待機・IQ再生中などの入力ロックは維持します。
- 「VFOを追加」は、処理待ちの操作を実行する時点の最新の受信中心へ新しいVFOを配置し、そのVFOを選択します。追加操作で既存VFOの周波数や設定を変更しません。基本値はWFM・200 kHzで、有効帯域とRF 0 Hz以上に収まるよう必要時だけ帯域幅を縮めます。中心10 kHzでは最大20 kHzです。これは配置条件であり、狭い帯域でFM放送を完全に復調できる保証ではありません。
- Spectrum・Density・Waterfallは同じ受信中心とSpanの周波数目盛りを使います。受信中心・Span・RF Gain等の入力条件や処理世代が変わった際は古い描画を破棄し、新しい条件のフレームへ切り替えます。
- Densityの縦軸上限・下限は独立した手動設定です。RF GainやSpectrumのRef Level/Rangeを変えても、この範囲を自動変更しません。Gainは「入力に適用」後の実効値を確認してください。AGC・Direct入力や離散的な対応gain値では、要求値と実効条件が異なる場合があります。

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

配布用ZIPを使う場合は、Windows 10/11 x64と通常のブラウザーを用意します。初回起動前に [Microsoft公式 Visual C++ v14 Redistributable（x64）](https://aka.ms/vc14/vc_redist.x64.exe) を導入または更新し、ZIPをすべて書込み可能な場所へ展開して`start-studio.cmd`を開いてください。公開ZIPには`vcruntime140.dll`を同梱しません。ビルドに使ったMSVCと同じかそれより新しい対応ランタイムが必要です。Rust・Python・Node・Build Toolsは実行先には不要です。ランタイム導入は利用者が行い、アプリからの自動インストールは行いません。

LIVE受信には受信機のInterface 0用WinUSBドライバー設定が別途必要です。保存IQの再生には受信機やWinUSBは不要ですが、アプリ用のMicrosoftランタイムは必要です。USBドライバーの導入や受信開始は自動実行しません。

ソースからビルドするPCには、RustのMSVCツールチェーンとMicrosoft C++ Build Toolsが必要です。

```powershell
.\build-studio.cmd
.\start-studio.cmd
```

受信DLLがなくても起動でき、画面には未接続状態が表示されます。公開プレビューの`rtlsdr.dll`は、RTL-SDR Blog **V1.4.0** と libusb **1.0.30** の固定ソースからビルドしたWindows x64版です。libusbを静的リンクし、Microsoft CRTを動的リンクしています。DLLのSHA256は `761de6ef2f57047f1dfbbf2346cb53247b2974f5d1fb11b64f9498b650cafc05`。ドライバーの対応ソースと再ビルド、パッケージ生成は [PACKAGING.md](docs/PACKAGING.md) を参照してください。WindowsのUSBドライバーのインストールや機器設定変更は行いません。

ネット接続なしでソースから再ビルドする場合は、同じリリースの対応ソースZIPを展開し、先に`SOURCE_BUILD.md`を確認してください。通常のGitHub source archiveとは別に、全66 Rust crateをvendorし、RTL/libusbのソースarchive・MSVC設定patch・再現手順を含めています。

起動時は受信・Arm・録音を停止しています。設定は`data/settings/`、新しいIQ/WAVは`data/recordings/`へ保存します。初回に同梱プリセットを読み込む場合は、受信停止中に`presets/fm-measurement.json`を設定プリセットとして配置するか、パッケージ生成時に初期値として同梱します。含まれる周波数やゲインは一つの実験条件で、屋外受信の万能設定ではありません。

```powershell
# サーバー・受信・録音を開始しない読み込み診断
native\target\release\rtl-spectrum-studio.exe --self-check --project-dir .
# ポートを変える場合
.\start-studio.cmd --port 8766
```

通常は127.0.0.1だけで待受けます。外部CDN等を使わず、ネット接続を必要としない構成です。持出先の実機環境での確認は未実施です。終了はヘッダーの「アプリ終了」で確認し、終了表示の後にタブを閉じます。コンソールから起動した場合はCtrl+Cでも正常終了できます。適用済み設定は通常の変更操作ごとに保存します。未適用の編集値を終了時に自動適用する動作はありません。

## テスト

```powershell
cargo test --manifest-path native\Cargo.toml --all-targets --jobs 2
node tests\ui-static-boundaries.cjs
node tests\ui-reception-boundaries.cjs
node --test tests\frontend-ui.test.cjs
node --test tests\audio-ui-state.test.cjs tests\app-shutdown.test.cjs tests\frequency-axes-pan.test.cjs
# Python参照テストを使う場合だけNumPyが必要
python -m unittest discover -s tests -v
```

最新仕様の追加純JavaScript検査は、音声状態9件（audio-ui-state）、終了処理9件（app-shutdown）、周波数目盛り・中心移動・VFO追加15件（frequency-axes-pan）の計33件です。既存回帰66件と合わせた構成は99件で、軸だけの自動承認と一般手入力D24の分離、最新中心へのVFO追加と新規選択、低中心周波数での帯域制限も対象です。最終候補での総合結果と実GUI・実機は [VALIDATION.md](docs/VALIDATION.md) へ分けて記録します。先行v0.4.1候補では追加29件＋回帰66件＝JS95件とNative108件が成功しましたが、その先行候補は今回の軸確認省略・新規VFO配置変更前です。

旧v0.4.0ではRust108件・JavaScript66件が合格しました。JavaScriptはブラウザー・ネットワーク・実機を使わない状態遷移テストで、中心補正の表示、録音中のロック、旧frame棄却、ピーク選局も含みます。保存実機IQを明示指定する追加テスト1件は通常の`cargo test`ではignoredになり、個人の録音データは同梱しません。数値境界と製品API・実機・GUIの結果は [VALIDATION.md](docs/VALIDATION.md) で区別します。

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

## 配布履歴

[v0.4.0-preview.1](https://github.com/ReiA-O/rtl-sdr-spectrum-studio/releases/tag/v0.4.0-preview.1) は最初のGPL公開版です。Windows ZIPは11,859,622bytes、SHA256 `18922b48928efece87f961585f80e1ee3c13403549a42cca6cec6f2b65cf568c`、対応source ZIPは20,020,517bytes、SHA256 `8452cc924ce0006d38e27efc616dfe83257cc4849a797c9dbabdcf1036729490`。当時のUSB境界24項目・公開ZIP再生14項目・対応sourceのオフラインビルドと匿名取得の整合を確認しました。この旧版の成功数・hashをv0.4.1の新しい実行物の結果にはしません。

2026-10-27の旧デモ同梱開発版 `RTL-SDR-Spectrum-Studio-0.4.0-dev-demo-windows-x64-20261027-210450-328.zip` は、11,296,451 bytes、SHA256 `d04cde25fc4e250837dd80ff4a5d5c19838f40261dd5906428aba0ec967892de` です。`v0.4.0-demo`のprivate下書きに保存した時点では、公開用ライセンスの選定が未完了でした。この履歴は保持し、現在の公開プレビューではGPL-3.0-or-laterの選定、対応ソースの同梱、ソースからビルドしたDLLへの変更、Microsoftランタイムの外部前提化を反映しています。旧ZIPのhashや検証結果を、公開プレビューZIPのhash・検証結果として扱わないでください。
