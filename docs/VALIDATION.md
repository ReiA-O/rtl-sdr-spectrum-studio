# 検証範囲

## v0.5.0-preview.1 — SSB・方式推定

アプリ内部版0.5.0／パッケージ版0.5.0-public-preview.1。2026-10-28に以下の最終実行物を確認しました。旧版の成功数を転用せず、実GUIと対応ソースの検証を分離しています。

最終ZIPは`RTL-SDR-Spectrum-Studio-0.5.0-public-preview.1-windows-x64-20261028-011740-215.zip`、11,907,069bytes、SHA256 `a3659e55e533022f6c159934e20367db4fb3e04d07679cfad89d9097c54a9520`。内包exeのSHA256は`3b2b1329d3bf35bc22d4351f6f9bf02ba930d00a32a18a269d2b545375eb01fb`です。推定パネルを開いたIQ再生でも全操作へ到達できるよう、サイドバー全体のスクロールを調整した版です。以下のREPLAY・方式推定・GUIはこの正確なexeで確認しました。公開通知を確定して再包装した際は、exe・driver・プリセット・デモ等94ファイルのbyte不変を照合し、通知と包装metadataだけの差を分離しました。最終ZIPも別展開して自己診断と停止起動を確認しています。

| 確認層 | 結果・限定範囲 |
|---|---|
| Native全対象 | library142＋main4＝146 pass、外部保存IQ fixture1件ignored。SSBの方式・帯域・RF 0 Hzと取得帯域端・AFC拒否・連動ROI・設定互換、推定の候補／保留・条件変更時の無効化等を含む |
| JavaScript | 124/124 pass。既存99＋SSB UI12＋推定UI13。公開用ソースのharnessで実行し、実行中のapp.js hash不変を確認。ブラウザー・ネットワーク・実機を使わない検査 |
| 正確Windows ZIP | 最終候補の全entry hash一致を確認。固定source-built driverとMicrosoft runtime別導入の構成を継続。自己診断で内蔵UI・DLL読込みとreceiver／server／recording未開始を確認 |
| SSB保存IQ・製品API | 最終exeで21項目成功。合成cu8の同じ原IQからUSB/LSBを再生し、片側RF帯域・AFC拒否・WAV確定・音声周波数・反対側の試験音・原IQ不変・停止状態を確認 |
| 方式推定・製品API | 最終exeで51項目成功、失敗・エラー0。同じAM IQをWFM/AM/USB/LSBで復調しても推定特徴が一致。FM候補、基準不明SSBの保留、明示基準に対するUSB/LSB候補、CW／雑音／混信の保留、不正設定拒否、停止・シーク・同調等での候補／基準解除を確認 |
| 実GUI | 同じ最終exeで16項目成功。USB／LSBの2.7kHz既定、SSB AFC適用外、停止警告解除、推定初期OFF、IQを開く／再生、USB復調中のAM候補、自動変更なし、明示クリックによるAM10kHz採用とVFO／受信中心の維持、スクロール、基準解除、EOF失効、音声OFF、正常終了を確認。実スピーカー聴取とは別 |

### 最終候補の合成SSBのWAV確認

| 選択方式 | 目的音の主成分 | 反対側の試験音／目的音 | WAV |
|---|---|---|---|
| USB | 1000.0 Hz | −83.3011 dB | 32 kHzモノラルPCM16、81,920samples、PCM端値0 |
| LSB | 1800.0 Hz | −64.0579 dB | 32 kHzモノラルPCM16、81,920samples、PCM端値0 |

このdB値は、WAVの開始0.4秒・末尾0.1秒を除きHann窓を掛け、目的音と反対側の試験音それぞれの±5 Hz内の最大FFT振幅から求めた比です。異なる音を両RF側へ置いた合成信号での通し確認で、実受信機の反対側波帯抑圧、RF感度・SINAD・音質の測定値ではありません。両方式とも原IQのhashは不変でした。

方式推定の51項目では、各候補と保留の後に復調方式・幅・VFO Hz・受信入力が自動変更されていないことを確認しました。FM候補はNFM/WFMの自動選択を行わず、明示した搬送波基準も停止・シーク等で解除します。固定された有限の合成IQでの検査であり、一般の信号に対する正答率や校正済み確率は算出していません。処理時間・負荷の観測もこのPCの全DSPブロックについての値で、推定機能単独の追加負荷や低性能PCでの保証ではありません。

### 修正と残る確認

- プリセットのVFO配列が不正な場合に、候補の入力条件を確定する前に拒否するよう検査順を修正しました。有効なVFOへ手動更新した後は、過去の停止理由を現在の停止状態として残さないようにしました。上記Native回帰の対象です。
- 推定パネルを開いた小さい画面やIQ再生でも全操作へ到達できるよう、サイドバー全体を縦スクロールできます。1280×720でIQ再生を閉じる操作まで到達し、専用アプリの正常終了・所有PID終了・ポート閉鎖を確認。全画面寸法の合格とは扱いません。利用者の別バージョンの受信は操作していません。
- 対応ソースZIPはこの文書を含む同一コミット、依存66crateとドライバー材料6点を固定します。空Cargo HOMEから同梱vendorだけを使う再ビルドの結果と正確なarchive hashは、Release添付の `RELEASE-VALIDATION.json` を参照してください。材料の一致を再ビルド成功やビット単位の再現保証へ読み替えません。
- ここに記載したSSB／推定の通し試験は保存IQの再生で、受信機やAFGを操作していません。実RFのSSB聴感・反対側波帯抑圧・混信下での推定精度、clean Windows／別PC／屋外／長時間・全RF帯域特性は未検証です。

対応ソースの検証結果は対象artifactのhashと対応させてReleaseへ添付します。上記の通し試験だけで、全実環境の受入れを完了扱いにしません。

SSBと方式推定の操作・制約は[MODULATION.md](MODULATION.md)を参照してください。以降はv0.4.1以前の成果物に対応する検証履歴です。

## v0.4.1-preview.1 — 操作フィードバック修正版

アプリ内部版0.4.1／配布版0.4.1-public-preview.1。音声の現在状態と操作の分離、正常終了、3画面の共通周波数軸と入力変更後の旧frame破棄を対象とします。最新仕様では周波数目盛りのdrag/同軸キー操作を事前承認済みとして扱い、帯域外VFOは確認ダイアログなしで停止します。SpanとVFO絶対Hzは固定し、一般手入力のD24確認・録音lock・Density独立dB上下限は維持します。

新規VFOはqueued追加処理の実行時点の最新中心へ配置し、その新VFOを選択します。既存VFOの設定は変えません。基本WFM/200 kHzで、有効帯域およびRF 0 Hz以上に収まる帯域へ縮めます。中心10 kHz時の最大20 kHzは配置の境界条件であり、FM放送の完全復調保証ではありません。

最新仕様の追加純JS検査の構成は音声9・終了9・周波数/Pan/VFO追加15＝33件（既存回帰66と全99件）です。以下の先行候補の結果は、今回の軸確認省略・新規VFO配置仕様変更前です。新しいexe/packageの実行検証と区別します。

### 最終v0.4.1パッケージの確認

- Native108（104library＋4main、外部保存IQ fixture1件ignored）／JavaScript99（旧66＋audio9＋shutdown9＋pan/VFO15）が成功。Nativeの機能sourceは旧0.4.0と同じで、UIと自package版を更新しています。
- 最終ZIPは`RTL-SDR-Spectrum-Studio-0.4.1-public-preview.1-windows-x64-20261027-233801-704.zip`、11,865,218bytes、SHA256 `4f59d29e5b53752b358766f738b4023bf1720256690f87edbaa0744d72370492`。exe SHA256 `aed72a0c3e1e5fea947954d7e4912cd6fcf037c5ee2cf61cc57caf287289eb99`。全entry/hash/privacy・別展開99files byte一致・self-check成功。
- その同じ最終exeで実機gain3cases／12checks・15frames成功。manual要求／reported actualは0／20.7／49.6dB一致。背景Spectrum Live中央値は−79.25798／−75.77226／−68.09814dBFS、Density列の最頻row中央値は−78.203125／−74.921875／−66.171875dBFS。統計が異なるため両値の一致は要求しません。AFG CH1 OFF・公称60dB減衰の既存接続、未校正の内部雑音相対観測で、RFgain差／SNR／感度の校正ではありません。AFG・配線・OSdriverは変更していません。
- 同じ最終exeのREPLAY14項目成功。WAV主成分1kHz・266,240samples・PCM端値0、元設定復帰、同梱assets hash不変。実スピーカー聴感ではありません。
- 実IAB GUI15項目成功。音声現在ON/OFFと次操作、音量0での手動切替、左右dragのSpan600kHz固定と既存VFO絶対Hz不変、帯域外VFOのdialogなし停止、新規VFO最新中心配置＋選択、3画面768周波数列共有、一般手入力だけの確認／取消、正常終了取消／実行を確認。
- GUI終了時のテスト用合成IQ32,768,000bytesとWAV256,000samples/32kHzはstop_reason=shutdownで確定。RIFFのframe数と実PCM byte長、SigMFの偶数byte数とmetadataを確認。終了表示・操作無効化、所有PID消失・8766閉鎖も確認。実測原データや配布ZIPを変更していません。

対応source ZIPは同じ最終commitを固定し、vendorとdriver sourceを含めて別添します。正確なsource hashと空Cargo HOMEでのoffline build結果はRelease添付の`RELEASE-VALIDATION.json`を参照してください。別PC／屋外／長時間／実聴・全RF帯域の校正は未検証です。

### 先行v0.4.1候補の結果（仕様変更前）

| 確認層 | 結果・限定範囲 |
|---|---|
| Native | library104＋main4＝108 pass、外部保存IQ fixture1件は明示選択用ignored。機能sourceは旧0.4.0から不変で、自package版だけ変更 |
| JavaScript | 旧回帰29＋20＋17と、新audio9／shutdown9／frequency axes/pan11＝95 pass。実GUIやHTTPとは別 |
| gain／density純release probe | 同じRust実装へ既知cu8振幅を入力し11項目pass。Spectrum−11.8128dBに対してDensity−11.25dB、差0.5628dBはdensity1row0.625dB以内。gain変更で履歴0・全cell0・epoch更新、IQ中gain変更lockを確認。実受信機のRF gain試験とは別 |
| 正確Windows ZIP | 全entry hash／privacy／source-built driver／CRT非同梱を確認、別展開99fileと元ZIPのbytes一致、self-check exit0。self-checkはreceiver／server／recordingを開始しない |

確認対象ZIPは `RTL-SDR-Spectrum-Studio-0.4.1-public-preview.1-windows-x64-20261027-231523-529.zip`、11,864,329bytes、SHA256 `f52bd90c8810d98f8206f0f4aba67e15eed740ec84062ec3cb3a9b11b88649f1`。exe SHA256 `bcfb5eced2cae5c9018a783c1ca4dfa7882e714dc8e05d9955be806d18b3b2d5`。driverは旧公開版と同じ761de6ef…、MSVC Runtimeは外部前提です。

Spectrum／Density／Waterfallの表示範囲と元周波数列を共通化しました。FFT binsは均等ですが、snapshotの集約列は非均等になる場合があります。熱画像のraster補間は代表600kHz／4096FFT／600pxで内部中心差最大275.10Hz＝約0.275pxで、全列の点ごとの完全一致は主張しません。RF分解能や校正精度の向上を意味しません。

この先行f52候補では、USBの手動gain 0/20.7/49.6 dBの有限3ケースと保存デモのREPLAY14項目が成功し、元設定へ復帰しました。これらは上記bcfb…exeとf52…ZIPの結果で、最新の軸確認省略・新VFO配置を含む最終exe/packageの成功ではありません。

最新仕様の最終候補について、実機gain適用・実GUI・正常終了・最終対応source buildの結果をこの節へ検証後に追記します。以降は旧v0.4.0の検証履歴です。旧版や先行候補の合格を新しい実行物の確認済みへ読み替えません。


## 公開プレビュー v0.4.0-preview.1 の配布検証

アプリ内部版は0.4.0、配布版は0.4.0-public-preview.1です。アプリexeは前のv0.4.0と同一SHA256で、今回の変更はGPLライセンス宣言、配布script・文書、source-built driver、Microsoft runtime別導入です。過去の本体検証を新しいDLL・配布構成の検証と混同しません。

| 対象 | 結果・限定範囲 |
|---|---|
| 新driverのsource build | RTL-SDR Blog V1.4.0＋libusb1.0.30。libusb静的・CRT /MD動的。x64、アプリが使う21exportを確認。別展開から同じrecipeでoffline再build成功。timestamp等のためbinary byte一致は主張しない |
| 新driverのUSB実行 | 24項目成功。auto23,999,999 Hz→Direct Q／24,000,000 Hz→tunerと補正適用状態、Fs1.024／2.4 MS/s、要求gain−10／60 dB→reported0／49.6 dB、AGC reported null、PPM±500で有限sample取得。24 MHz境界は各2,228,224samples。AFG CH1 OFF・公称60 dB減衰経由、配線やAFG操作なし |
| packager契約 | 128項目成功。public/private、新driverの正確な6source材料・hash・provenance、CRT非同梱、GPL本文、外部prerequisiteを確認。source欠落／改竄／余剰、DLL改竄、不明依存等を拒否。private既定の旧同梱bytesを保持。既存demo契約67項目も成功 |
| 正確な公開ZIP | 下記ZIPの99entry CRC、manifest97filesの全bytes／SHA256一致・余剰なし、CRT非同梱・GPL・new driver・6source材料を確認。self-check exit0、内包UI／DLL読込成功。self-checkはreceiver／server／recordingを開始しない |

正確な公開ZIPから展開した専用runtimeでREPLAY14項目も成功。IQ一覧・専用preset・WFM WAV主成分1000 Hz・PCM transport・EOF／seek／停止・設定復帰・同梱assets不変を確認しました。音声の数値／輸送経路の検査で、スピーカーの実聴ではありません。専用serverは終了し、元アプリは停止状態を保持しました。

公開Windows ZIPは`RTL-SDR-Spectrum-Studio-0.4.0-public-preview.1-windows-x64-20261027-220010-601.zip`、11,859,622bytes、SHA256 `18922b48928efece87f961585f80e1ee3c13403549a42cca6cec6f2b65cf568c`です。driver SHA256は`761de6ef2f57047f1dfbbf2346cb53247b2974f5d1fb11b64f9498b650cafc05`。Microsoft公式Visual C++ v14 Runtime x64を別途導入する構成で、対応runtime導入済みのこのPCで実行しています。

新driverの初期化中にはPLL not lockedログが出る経路もあります。sample取得・software読戻しをアナログPLL lock／実gainの独立測定として扱いません。新driverでのRF全域感度・SINAD・SFDR・圧縮・鏡像・平坦度・温度依存、clean Windows／別PC／屋外／実スピーカー聴感／長時間は未検証です。元アプリ・元設定・通常録音を保持し、専用検証runtimeを使っています。

対応source ZIPは同じ公開commitから作り、全66crateとdriver全source／patch／recipeを含めます。最終source ZIPのhash・offline buildと正確公開ZIPのREPLAY結果は、[同じRelease](https://github.com/ReiA-O/rtl-sdr-spectrum-studio/releases/tag/v0.4.0-preview.1)の検証記録と照合してください。下段は旧privateデモ版と以前の本体試験の履歴です。


## 旧private AFG実録IQデモ同梱版（履歴）

AFG3102の32 MHz FM（偏移±75 kHz、1 kHz試験音）をStudioで収録し、明示承認されたIQだけを同梱した配布構成です。本体exeは既存v0.4.0と同一hashで、以下は従来のv0.4.0本体の合格数とは別に管理します。

| 対象 | 確認する内容 | 結果・範囲 |
|---|---|---|
| AFG実録IQ | 有限収録、保存完了・欠落・入力端値、復調音 | 8.32秒・Fs1.024 MS/s・17,039,360 bytes。欠落0、ADC端値到達0。収録WAVの主成分1 kHz、PCM端値到達0を数値確認 |
| 共有metadataと承認ファイル | 元IQとのbyte一致、meta/preset/READMEの個人情報除外、正確なサイズ・SHA256 manifest | raw byte保持、時刻／機器識別／ローカルパスを除外、sample起点0へ再構成。承認4ファイルを同梱 |
| package契約・export | manifest経由の厳密な同梱と個人data除外 | package契約67件・export10件が成功。未知path・サイズ／hash不一致等の拒否を含む |
| 最終デモ同梱ZIP | IQ pair・専用プリセット・案内、全entry hash・privacy・自己診断 | 下記210450-328の最終ZIPで成功。Rust／webの本体コードは更新していない |
| 受信機相当が利用できない状態 | 起動→プリセット→保存データ→REPLAY、シーク／EOF／復調音の経路 | 最終ZIPの受信DLLを無効化した模擬状態で14項目成功。1 kHz WAVとspeaker用PCM transportを含むが、実際のスピーカー聴取・物理USB抜去・清浄な別PCの試験ではない |
| 実GUI | プリセット、dialogを閉じる、IQ一覧、再生、4パネル、EOF／先頭、元設定復帰 | 先行デモ候補205550の実IABで9項目成功、console error/warn0。最終版の差は案内の×操作追記だけで、exe／IQ／metadata／presetの4点は同一hash |

最終配布ZIPは`RTL-SDR-Spectrum-Studio-0.4.0-dev-demo-windows-x64-20261027-210450-328.zip`、11,296,451 bytes、SHA256 `d04cde25fc4e250837dd80ff4a5d5c19838f40261dd5906428aba0ec967892de`です。privateの下書きReleaseへ保存済みで、GitHub asset digest・sizeもローカルZIPと一致しています。公開化はしていません。

原IQ SHA256は`98eeac276c5b39c0954a6b58cf032a6e2e2e3dae75b4cdcc7e18139f1d97483a`です。収録音の簡易tone比は53.50 dBでした。この指標は校正SNR／SINADや普遍的な受信性能ではなく、特定の試験音・接続・受信条件の数値確認です。受信機不使用の再生試験を、清浄な別PC・実際の聴感・屋外受信の確認へ拡張しません。

操作は「プリセット」から`AFG_FM_32MHz_demo`読込み→**設定画面右上の×で閉じる**→「保存データ」からデモの「IQ再生」→「再生開始」です。音声は利用者操作でONにします。EOF後は再生位置スライダーを先頭へ戻します。先頭への移動はGUI検査でスライダーのHomeキー操作も確認しました。

## v0.4.0の確認状況

中心DC補正、共通の補正後IQ経路、原IQ保持、ピークからのVFO同調、初期受信モード確定順序を追加した版です。2026-10-27の最終ビルドで以下の限定範囲を確認しました。下記の中間・前版結果とは区別します。

| 対象 | この版で確認する項目 | 最終状態 |
|---|---|---|
| Native全対象 | 補正APIの型・未知キー拒否、既定ON／旧設定、同値保持、Arm／IQ／WAVロック、履歴・音声の初期化、Direct／低Fs再生の迂回、入れ子の入力経路、校正表示、原IQ byte保持と入力・ナイキスト端境界 | library104＋main4＝108件合格。外部IQ fixture1件は通常ignored |
| UI状態境界 | 中心補正の要求／適用／停止の区別、録音中ロック、旧frame棄却、ピーク選局の帯域・編集中・通信失敗境界と旧回帰 | 匿名化exportで29＋20＋17＝66件合格。純JSで実GUI／APIとは別 |
| Release実装のDSP数値境界 | 実際のrelease Rust実装へ数値probeを実行。全ナイキストROIの端bin積分とMarker aliasを含む | 211項目合格、失敗0。対象probeのナイキストCP誤差0.0 dB。RF精度試験ではない |
| 実行中の入力設定境界 | 不正API15件の拒否・状態維持と、autoモードの24 MHz境界、Fs両端、ゲイン要求両端、AGC、PPM両端の受信 | 計24項目合格。設定API／mode・Fs等の読戻し／有限sample取得で、RF全域特性やAGC実応答の評価ではない |
| 保存IQ・製品API | 同じ原IQのON／OFF再生、ピーク・復調・トリガ、IQ/WAVの経路記録と原ファイル保持、設定復帰 | 最終配布物のAPI19項目合格。合成中心約65.15 dB、保存50Ω入力のLive約12.08 dB／Max約11.23 dBの低下、AM 1 kHz WAVと原IQ区間byte一致を確認 |
| 実機中心成分の相対比較 | 50Ω終端のON／OFF／ON、中心±1 kHzの最大表示値と20～200 kHzの背景 | 下記条件の有限比較を完了。校正S/N・感度・原因解消の証明ではない |
| 実GUIと配布構成 | 最終配布物のDEMOでマーカー選局、条件保持、補正切替・ロック・履歴、停止状態、設定復帰。release／内蔵UI／DLL診断とZIP全entry／privacy | GUI12項目合格、console warn/error0。release・self-check・ZIP検査成功。別PC／実聴は未確認 |

テスト用の合成IQや保存IQの処理が成功しても、RF全周波数域の感度・SINAD・SFDR・alias・絶対レベル・捕捉率・AGC実応答・長時間安定性・屋外運用の保証にはしません。公開用のソースには、追加で明示承認するAFGデモIQを除いて元の実収録・検証ログ・個人設定を含めません。

### 最終実機・GUIの条件と結果

- 50Ω終端で補正ON／OFF／ONを比較。各条件6観測の中心±1 kHz最大表示値の中央値は、順に−72.59144／−56.56955／−74.80480 dBFSでした。OFFから最後のONまで18.23525 dB低下しました。前後のONにも差があり、固定の抑圧性能保証値にはしません。
- 中心から20～200 kHzの背景中央値はOFF −74.96397からON −74.83575 dBFSへ、約＋0.12822 dB変化しました。有限の雑音統計を含む差で、RFのS/N・雑音指数・感度を測定した結果ではありません。
- auto入力の23,999,999 HzではDirect Q、24,000,000 Hzではチューナーとなり、各2,228,224 samplesを取得しました。Fs 1.024／2.4 MS/s、ゲイン要求−10／60 dBでは実効表示0／49.6 dB、PPM±500を含む条件でも有限sample取得を確認しました。AGCの実効読戻しはnullで、設定成功と制御実応答の測定を混同しません。
- 最終GUI12項目はDEMO入力です。自動／ユーザーマーカーからのVFO選局と読戻し、中心・mode・帯域幅保持、補正ON／OFFの履歴初期化・ロック、停止中の表示、元プリセット復帰を確認しました。実RF選局やスピーカー聴感の合格ではありません。
- 検証後は元設定へ復帰し、LIVE選択・受信停止、Arm／IQ／WAV録音なしです。中心AM局の搬送波抑圧による復調の崩れは仕様上の副作用として残り、OFF比較か受信中心を局からずらす運用を使います。

デモ追加前の確認対象ZIPは`RTL-SDR-Spectrum-Studio-0.4.0-dev-windows-x64-20261027-201750-566.zip`、2,039,603 bytes、SHA256 `6dcb96a2ff2aeacff60d65a13fbc651ea2ece50afa3db3c0b8bc8743334b1756`です。ZIP全entryと個人文字列検査・自己診断の成功は、清浄な別PCの受入れ完了とは区別します。

デモ追加前のv0.4.0公開準備では、明示allowlist 49ファイルを検査し、変更対象を含む27ファイルで開発元とのbyte hash一致を確認しています。公開用のパスへ調整したJS試験66件とパッケージ生成スクリプトの構文確認も成功しました。個人文字列・LAN接続先・機器識別値・認証文字列の走査で検出はありません。これは新しいcloneからのRust再ビルド、ダウンロード・パッケージ生成の通し試験、清浄な別PCの確認とは区別します。

### v0.4.0中間ビルドの結果

境界修正前の2026-10-27 19:49ビルドで確認した限定結果です。最終配布物の再試験とは区別します。

- 保存IQを使うAPI19項目が合格。同じ合成rawの中心成分はONで約65.15 dB減り、AM 1 kHzの復調WAVと原IQ byte一致を確認しました。保存済み50Ω入力の比較では約12.08 dBでした。一定の実機抑圧保証値にはしません。
- 50Ω終端の実機でON／OFF／ONを比較。中心±1 kHzの最大表示値を各6観測で比較した中央値は、OFF −56.685 dBFSからON −72.324 dBFSへ約15.64 dB低下しました。中心から20～200 kHzの背景中央値の差は約−0.042 dBでした。有限・未校正の相対表示比較で、雑音指数やRFのS/N・感度改善を測定した結果ではありません。
- DEMOを用いた実GUI12項目が合格。自動／ユーザーマーカーからの選局、mode／帯域幅／受信中心の保持、補正チェックのロック、OFF時履歴初期化、停止状態の表示を確認し、console warn/errorは0でした。実RF選局や聴感の合格にはしません。
- その後の境界レビューで、明示チューナーモードと低い中心周波数の組合せに、設定計画の受理と機器モードの不一致がありました。最終版では受信停止より前に拒否するよう修正し、上記の単体・実行中APIの境界試験で再確認しました。
- 追加の数値境界レビューでは、全ナイキスト帯域をROIにした際、端にある成分のChannel Powerが半分（約−3 dB）になる不具合を確認し、周回する境界binの積分を修正しました。上側端だけのROIでのMarker表示周波数も修正し、上記の追加境界試験とrelease DSP probeで確認しました。直線周波数軸のOBWが両端間を含む制約は残ります。

### 数値境界probeの再現

再現用sourceは [electrical_boundaries.rs](../native/examples/electrical_boundaries.rs) です。実際のRustライブラリーへ合成IQを入力し、中心補正の伝達特性、FFT／RBW／帯域端、ナイキストROI、AM/FM復調、デシメーション、PCM飽和等を検査します。DLL・サーバー・実機・個人の収録データは使いません。

```powershell
cargo run --release --manifest-path native/Cargo.toml --example electrical_boundaries -- <新規出力フォルダー>/result.json
```

`<新規出力フォルダー>`は毎回新しい実パスへ置き換えてください。例えば`verification/electrical-probe-001/result.json`のように、Git対象外の`verification/`や`data/`内へ出力します。JSONと同じフォルダーの下にPCM検査用WAVも作成します。同名JSONを上書きしないため、既存の出力先への再実行は失敗します。新規フォルダーと新規JSON名を使い、結果JSON／WAVをGitへ追加しないでください。

## v0.3.0以前の確認記録

2026-10-27時点の開発記録を、個人データ・機器識別情報を除いて要約しています。原IQ、実機接続先、シリアル番号、私的な検証ログや画面は同梱しません。前版v0.2.2はRust63件・JavaScript29件が合格。今回v0.3.0はRustの通常全対象82件と追加I/O境界の対象指定1件が合格しています。JavaScriptは既存29件＋受信音声補助20件＝49件が合格しています。追加I/Oの1件を全対象再実行の合格数へ混ぜません。

Python参照32件は前版の結果で再実行していません。Densityだけの履歴初期化と密度／全体FFT数の分離、および今回のAFC・受信音声補助はRust側の実装で、Pythonへは未移植です。公開準備用の別ソースフォルダーでは今回の変更source10ファイルのhash一致とJavaScript49件を確認しましたが、別PC・新しいclone環境の検証へは読み替えません。v0.3.0配布版の保存実機IQを使うHTTP API・WAV検証13項目は合格。配布版実GUI12項目と受信・WAV読戻し12項目も合格。スピーカー聴取は未確認です。

| 対象 | 確認済みの範囲 | 限界・未確認 |
|---|---|---|
| Rust本体 | library78＋main4＝82項目の全対象試験が合格。追加I/O境界1件も対象指定で合格。既存の保持リセット・Density・入力境界に加え、受信音声補助の統合を確認 | 明示IQ fixture1件は通常ignored。合成信号とEngineのDEMO／REPLAYを含む試験で、全実機条件ではない。追加1件後の全対象再実行とは区別 |
| Python参照 | 32項目合格 | 数値参照用。Rustの実装と性能・FFT精度が完全一致する保証ではない |
| JavaScript | 49項目合格。既存のリセット、Density入力境界、校正変更、dirty入力・応答待ち中の追加入力保持、旧frame棄却と、受信音声補助の状態境界を含む | pure JavaScriptのテストで、ブラウザー表示・クリック・音声再生の検証ではない |
| v0.3.0受信音声補助 | 合成AM/NFM/WFMの小さな周波数ずれ追従、AFC ON→OFFで実補正0・名目周波数保持、品質不足／DC／帯域端での保留、上限・位相・chunk境界、RFスケルチ／hang、gain／limiter／fade、WAV経路・分割・統計・旧設定復元をnativeで確認 | 実APIとGUIは後述の限定範囲を確認。新規実RFでのAFC追従／音質・スピーカー聴取は未確認。既存の実機試験を新版の補助機能の合格とはしない |
| v0.3.0保存IQの実API | 配布版で同じ保存WFM IQを6条件再生し13項目合格。復調WAVのgain0/18 PCM一致、処理後limiter上限、SQL開閉・4ms fade、未補正PSD一致、元IQ/meta不変と原設定復帰を確認 | 新RF収録・実AFC追従／聴取品質の実機評価ではない。WAV開始までの遅れを共通source sampleで整列し、処理branch統計と保存PCM経路を区別 |
| v0.3.0実GUI | 12項目＋受信・WAV読戻し12項目成功。AFC手動ON/OFFとOFF補正0、補助設定・不正値拒否・未適用編集保持、録音中経路変更の分割、原設定完全復帰・旧リセット/Density継続、console0 | AFC ONは信号不足でLOCKなし。帯域端の実GUI・実聴・全画面幅や全組合せの合格ではない |
| 実機の基本経路 | 13項目の操作・IQ/WAV保存・既知1 kHz復調等を確認 | 一部IQ再生は合成入力由来。全体の感度・音質・校正合格ではない |
| ユーザーマーカー | 保存実機IQでEngine API経由の7項目を確認。独立f64 FFTと周波数・レベルを照合 | 保存データでのオフライン検証。新LIVE受信や実画面操作の検証ではない |
| 受信条件境界 | ゲイン両端・帯域端付近・サンプルレート等8条件の有限収録と解析 | 設定値・雑音・実効RBWの影響を含む。RF入力レベルは未校正 |
| 配布構成 | remap release、Windows x64 PE依存、内蔵UI、全ZIP hash・個人文字列検査、日本語と空白のある別フォルダーの起動cmdから内蔵UI／package内DLL読込み診断成功。DLLの長いパス処理も修正 | 診断はサーバー・受信・録音を開始しない。未署名の開発版で、別PC／実GUIとは区別 |
| 画面・持出し | v0.2.0配布版の代表23項目に加え、v0.2.1配布版で実GUI6項目＋読取りAPI継続6項目。停止中／LIVEのリセットと3連打、Trace再描画、受信・密度・Waterfall・時計継続、設定復帰、console warn/error0を確認 | 実GUIでの録音中クリック、全操作、ノートPC、屋外、清浄な別PC、スピーカー聴取は別途確認 |
| Density手動範囲 | v0.2.2配布版で実GUI8項目＋読取りAPI8項目。上下限適用、密度だけ再集計、Spectrum／Waterfall／全体FFT／時計継続、同値再送・空／逆転拒否、元設定復元、console warn/error0を確認 | 実GUIのdBm校正と狭幅画面は未実施。校正と編集中の単位変化はnative／純JSの範囲 |

中心追従ピークは、受信IQの複素平均と背景で定量的に説明できることを確認しています。発振源出力ON/OFF比較でも中心成分が残りましたが、ADC・ベースバンド・DDC・ドライバー処理・中点規約の物理的な発生箇所を一意に特定したわけではありません。平均値を引いて消えることだけを物理原因の証明やS/N改善としません。

GainやFsの比較でdBFS値は変わります。Fs 2.4 MS/sの実効ENBWが439.45 Hz、他の比較条件では750 Hzとなったため、雑音密度はdBFS/Hzで区別しました。低S/NのOBWは雑音で測定帯域幅へ近づき、変調波固有の幅とは解釈できません。

実RF入力の絶対電力、減衰器の実特性、周波数基準、校正dBm、感度、SINAD、捕捉率、保証VFO数、長時間連続運転は未確定です。初期要件全体の受入れ合格や測定器同等性能とは扱いません。

再実行する通常テストはREADMEのコマンドを使います。保存実機IQを使う追加テストには、利用者が自分のデータと必要な比較／プリセットJSONを明示指定する必要があります。原データを公開する必要はありません。
