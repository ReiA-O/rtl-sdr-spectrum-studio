# 試験報告: 停止時の flush・再生の巻き戻し・capture gap（2026-11-02）

CI を使わないローカル実行の記録です。[VALIDATION.md](../VALIDATION.md) の方針に従い、実行ログそのものはリポジトリに含めず、チームの非公開の作業記録に保存しています。ここにはその要約を置きます。

## 対象

- ブランチ `kuro/rtl-residual-tests`（`origin/main` の `d264dbd` から分岐）。この報告を含むコミットで実行
- 含まれる変更: PR #1（停止時の flush、停止時の巻き戻し）、PR #2（空 IQ の拒否、末尾からの再スタート）、今回の試験とコメント、flush を繰り返し呼んだときの修正

## 実行環境

- OS: Microsoft Windows 11 Pro 10.0.26200
- rustc 1.99.1 (58cf8f9c3 2026-10-13)
- cargo 1.99.1 (5f94df478 2026-08-27)
- debug ビルド（`test` profile）、作業フォルダーは `native/`

## 実行コマンドと結果

| コマンド（`native/` で実行） | 結果 |
|---|---|
| `cargo test --locked -- --test-threads=2` | lib 154 passed / 0 failed / 0 ignored、main 4 passed、`tests/offline_user_marker_fixture.rs` 0 passed / 1 ignored（既存。実収録 IQ が必要）、doc 0。終了コード 0 |
| `cargo test --locked --lib flushed_analog_tail -- --nocapture --test-threads=2` | 1 passed。実測値は下の表 |
| 修正前の `cargo test --locked --lib repeated_flush_without_reset -- --ignored --test-threads=2` | 1 failed（2 回目の flush で AM 29 サンプル・ピーク 1.0 が出た。修正後は通常のテストとして合格） |
| `cargo test --locked --lib runtime::tests::replay_ -- --test-threads=2` を 30 回 | 30/30 合格 |
| `cargo test --locked -- --test-threads=2`（全体）を 10 回 | 10/10 合格（他の DSP テストと CPU を取り合う状態での runtime テスト） |

## 区別して確かめたこと

### flush の範囲と再生 queue

- `VfoDemodulator::flush()` が出すのは群遅延 D の分だけです。FIR の履歴全体（taps-1）は回収しません。出力は ceil((N+D)·ar/fs) で打ち切ります。これより後の音声は最後の実サンプルより後の時刻を表し、ゼロ埋め由来になるためです（`native/src/dsp.rs` の doc コメント）。
- 再生 queue は flush の対象外です。`Engine::on_stop` は flush の前に `audio_chunks`（ブラウザーへ渡す再生用の音声）を空にし、`audio_epoch` を進めます。ブラウザー側（`web/app.js`）は epoch が変わると再生バッファーを捨てます。flush した末尾は WAV にだけ書かれます（`native/src/engine.rs` のコメント）。

### テスト一覧

| 由来 | テスト | 確かめる内容 |
|---|---|---|
| PR #1 | `dsp::tests::flush_emits_the_filter_tail_without_padding_audio` | SSB で末尾のバーストが flush で出ること。総数が上限ちょうどであること。AM/NFM/WFM の末尾が有限であること |
| PR #1 | `engine::tests::closing_stop_writes_demodulator_tail_to_wav_but_input_change_does_not` | 閉じる停止では末尾を WAV に書き、上限を超えないこと。`input_change` では書かないこと |
| PR #1 | `runtime::tests::replay_stop_while_pacing_or_after_read_keeps_every_sample` | ペース待ち中の停止や読み取り直後の停止で、読み取り位置が巻き戻ること。全バイトが順に読めること |
| PR #2 | `recording::tests::replay_rejects_empty_iq_data` | サンプル 0 件の IQ と半端なバイトを拒否すること |
| PR #2 | `runtime::tests::replay_start_after_end_rewinds_and_reads_every_sample` | 末尾まで再生した後の開始で先頭から再生し、全サンプルを処理すること |
| PR #2 | `runtime::tests::replay_start_after_mid_stop_resumes_in_place` | 途中で停止した後の開始では、その位置から再開すること |
| 今回 | `dsp::tests::flush_after_reset_or_idle_is_empty_and_one_flush_stops_at_the_limit` | 入力なしや reset 後の flush が空であること。1 回の flush で総数が上限ちょうど（AM/NFM/WFM/USB）になること |
| 今回 | `dsp::tests::repeated_flush_without_reset_adds_no_audio` | reset せずに 2 回目を呼んでも何も出ないこと（今回の修正で満たす） |
| 今回 | `dsp::tests::flushed_analog_tail_matches_continued_demodulation_until_the_last_quarter_millisecond` | AM/NFM/WFM の末尾波形（下記） |
| 今回 | `engine::tests::closing_stop_drains_the_tail_once_and_only_into_wav` | 末尾が WAV にだけ入ること。`audio_chunks` は空のままで sequence も変わらないこと。2 回目の `on_stop` では WAV が増えないこと |
| 今回 | `runtime::tests::replay_rewind_around_a_capture_gap_keeps_boundary_start_and_gap_detection` | SigMF の 2 capture（`core:sample_start` の間に 12,345 サンプルの欠落あり）で次を確かめる。境界の手前で切れたブロックは境界を越えて巻き戻らないこと。境界で停止しても位置は境界のままで、読み直したときの start が global index と一致すること。境界の手前のブロックを戻してから再開しても gap を 1 回検出すること。境界の後のブロックを戻してから再開すると start が正しいこと |
| 今回 | `runtime::tests::replay_stop_start_cycles_lose_no_block_and_finish_every_sample` | `Runtime` で再生中に stop/start を 4 回繰り返す。各停止の後、読み取り位置が「最後に処理したブロックの末尾」と一致すること（読んだブロックが失われていないこと）。最後まで流すと `input_samples` が総数になり、エラーが出ず、終了通知が 1 回であること |

### AM/FM 末尾波形の閾値と実測値

信号は fs=256 kHz、0.5 秒、1 kHz の変調です。同じ信号を 0.1 秒長く続けて復調したものを基準にしました。基準は入力の終わりの影響を受けません。

| モード | 末尾サンプル数 | 定常 RMS（理論値） | 末尾誤差の最大 / 定常 RMS（閾値 0.1） | 最後の 2 ms の RMS / 定常（閾値 0.9〜1.15） | 最後の 0.25 ms のピーク / 定常ピーク（閾値 4） |
|---|---|---|---|---|---|
| AM | 30 | 0.2133（0.2121） | 0.0271 | 1.0386 | 1.552 |
| NFM | 19 | 0.3536（0.3536） | 0.0016 | 1.0844 | 2.527 |
| WFM | 44 | 0.1352（0.1349） | 0.0042 | 0.9987 | 0.847 |

閾値の根拠:

- 定常 RMS の理論値は次のとおりで、許容差は ±2% です。AM は変調度 0.3、NFM は 2.5 kHz / 5 kHz、WFM は 30 kHz / 75 kHz に、50 µs ディエンファシスの 1 kHz での利得（0.954）と WFM 出力の倍率 0.5 を掛けた値です。
- DC 除去の時定数は 50 ms です。末尾は最長 1.4 ms（WFM の群遅延）なので、IIR の状態の動きは端の偏差の 1-exp(-1.4/50)≈2.8% 以内に収まります。最後の 0.25 ms を除けば、基準との差は定常 RMS の 10% 未満と見込めます。実測の最大は 2.7% です。
- 0.25 ms（32 kHz で 8 サンプル）は、チャネル FIR（遮断 6 kHz 以上、主ローブの半幅 83 µs 以下）と音声 FIR（遮断 4.8 kHz 以上、104 µs 以下）の主ローブより後ろです。この区間は入力が途切れたこと自体による過渡なので、発散していないことだけを確かめます（ピークが定常ピークの 4 倍未満）。
- 最後の 2 ms（1 kHz の 2 周期）の RMS は、末尾全体と端の過渡を含みます。端の過渡があっても 15% 以内に収まることを確かめます。

## 既知の制約と未修正の事項

1. **reset せずに flush を 2 回呼ぶと、2 回目でも音声が出ていた（修正済み）。** 2 回目の上限が、1 回目に入れたゼロ IQ の分だけ後ろへずれていました（実測は AM 29 サンプル、ピーク 1.0）。`Engine::on_stop` は flush の直後に reset するので、それまでの呼び出し経路では起きていません。ゼロ埋めを入力数に数えないように直し、2 回目は空になりました。flush の後に新しい入力を入れる前には、今までどおり reset() が必要です。
2. **末尾の最後の 0.25 ms には打ち切りの過渡がある。** NFM ではピークが約 1.26 になり、full scale を超えます。PCM 保存側で `demod_overrange_samples` として数えられ、クリップされます。
3. **`input_change` の境目では flush しない。** WAV のセグメントは開いたまま続くので、境目の直前の群遅延分（設定によります。今回の試験の USB の設定では約 10 ms、AM/FM では約 0.6〜1.4 ms）は WAV に入りません。運用で対応します（PR #1 からの仕様）。
4. **停止時の末尾は再生（スピーカー）には出さない。** `audio_chunks` を消し、ブラウザーも epoch の変化で再生バッファーを捨てます。
5. **capture の境界ちょうどで停止して再開すると、その gap は再通知されない。** `on_start` が DSP と gap の追跡をリセットするためです。読み取り位置と global index は正しいままで、gap は SigMF の metadata と最初の再生での通知に残ります。
6. **worker 競合テストでは、どの瞬間に停止が当たるか（ペース待ち中か、読み取り直後か、処理後か）がタイミングで決まる。** それぞれの経路は Source 単位の決定的なテスト（PR #1 と今回の capture gap テスト）で確かめています。待ちは状態のポーリングで行い、20 秒の上限を付けています。
7. 実行したのはこの PC の debug ビルドだけです。CI、release ビルド、実機（RTL-SDR）での確認はしていません。
