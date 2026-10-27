# RTL-SDR Spectrum Studio

RTL-SDRでAM/FM放送のIQ取得・音声復調・録音と、受信帯域の観測を行うWindows向け開発版です。Rust本体がローカルHTTPサーバーを動かし、exeに内蔵した画面を通常のブラウザーへ表示します。Pythonは数値比較・単体テスト用の参照実装で、アプリの実行には使いません。

現在はレビュー用のソース一式です。公開・販売向けのライセンス選定や製品全体の受入れ完了を意味しません。[LICENSE.md](LICENSE.md) と [検証範囲](docs/VALIDATION.md) を先に確認してください。

## 主な機能

- Spectrum・密度表示・Waterfall・複数VFOを同時表示。共通FFTから全有効フレームを集計します。
- AM/NFM/WFMモノラル復調、選択VFOだけのスピーカー出力、VFO別WAV録音。
- 中心周波数・Span・RBW/FFT・Ref Level・Rangeの調整、TraceとPersistence、測定用プリセット。
- Spectrumの「ピークリセット」でMax／Min／Averageを同時にリセットし、受信や録音を続けたまま現在のLiveから再集計。
- Densityの上限・下限を手動設定。Spectrumの表示範囲から独立し、密度履歴だけを新範囲で再集計。
- 自動ピークマーカーとユーザーマーカー、Channel Power、測定帯域内のOBW。
- 生IQの手動保存、レベルトリガ、秒単位のプリ／ポストトリガ、保存IQの再生。

詳しい動作と制約は [SPEC.md](docs/SPEC.md) にあります。密度表示はソフトウェアのFFT集計であり、専用測定器の捕捉率・測定精度と同等という意味ではありません。

継続確認が必要な点は [KNOWN_LIMITS.md](docs/KNOWN_LIMITS.md) に整理しています。

## 実行・ビルド

配布用ZIPを使う場合は、Windows 10/11 x64と通常のブラウザーを用意し、ZIPをすべて書込み可能な場所へ展開して`start-studio.cmd`を開きます。Rust・Python・Node・Build Toolsの追加は不要です。別PCでは受信機のInterface 0用WinUSBドライバー設定が必要です。ドライバーの導入や受信開始は自動実行しません。

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
# Python参照テストを使う場合だけNumPyが必要
python -m unittest discover -s tests -v
```

JavaScriptテストはブラウザー・ネットワーク・実機を使わない29項目の状態遷移テストです。Rustはlibrary59＋main4＝63項目。保存実機IQを明示指定する追加テスト1件は通常の`cargo test`ではignoredになり、個人の録音データは同梱しません。

## 構成

- `native/`: Rustの受信・解析・録音・設定・HTTP本体とテスト。
- `web/`: exeへ内蔵する画面。
- `studio/`, `tests/`: 数値参照用Pythonと単体テスト、JavaScriptの状態境界テスト。
- `presets/`: 個人情報を含まない測定用プリセット。
- `tools/`: 明示実行のDLL取得・パッケージ生成。機器操作用の実験スクリプトは含めません。

録音・個人設定・校正情報・機器IP／シリアル番号・開発環境の絶対パス・実験記録はリポジトリに含めません。更新版は別フォルダーへ展開し、既存の`data/`を上書き・削除しないでください。
