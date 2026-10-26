# Third-party notices for the private portable development build

本パッケージは個人用のWindows PCへ持ち出すための開発版。外部への公開・販売は行っていない。本体の公開用ライセンス選定や法的適合性の包括的な確認を完了したという表示ではない。

- **RTL-SDR Blog driver V1.4.0**: rtlsdr.dll は未変更の公式Windows x64 release由来。GPL-2.0-or-later。COPYING とV1.4.0のソースアーカイブを同梱する。ReleaseのSHA256とDLLの一致をpackagerで検査する。
  - Release: https://github.com/rtlsdrblog/rtl-sdr-blog/releases/tag/V1.4.0
  - Source: https://github.com/rtlsdrblog/rtl-sdr-blog/tree/V1.4.0
- **libusb**: rtlsdr.dll内にlibusbの実装が含まれることを文字列と依存関係から確認した。外部libusb DLLは必要ない。LGPL-2.1-or-laterの本文を同梱。公式releaseバイナリー内の正確なlibusb版・ビルド設定は未確認で、公開配布前には対応ソースとビルド由来を確認する必要がある。
  - https://github.com/libusb/libusb
- **Microsoft Visual C++ runtime**: vcruntime140.dll は正規インストール済み Visual Studio Build Tools の VC/Redist/MSVC/x64 から取得し、アプリの隣に配置する。Microsoftのライセンス条件に従う。この配布用フォルダーだけを使用し、Windows/System32のDLLを抜き出さない。参照用のMicrosoft Visual C++ Runtime 2015-2022 License文書を同梱する。実際のファイル版はmanifestに記録する。
  - https://learn.microsoft.com/en-us/cpp/windows/redistributing-visual-cpp-files
  - https://visualstudio.microsoft.com/license-terms/vs2022-cruntime/
- **Rust standard library**: 使用したツールチェーンの COPYRIGHT-library.html とライセンス本文を同梱する。
- **Rust crates**: Cargo.lockに対応する Windows x64 の依存グラフ（build/proc-macro依存も含む）から、各crateの原ライセンス・著作権通知を licenses/rust-crates/ へ保全する。一覧と宣言は rust-crates.json を参照する。関係のない他OS専用のlock entryは含めない。

署名付きインストーラー、独立したクリーンPCでの受入れ、外部公開は今回の成果に含めない。Windowsに標準搭載のUCRT/API-set DLLは同梱しない。WinUSB USBドライバーは別途必要で、パッケージは導入や変更を自動実行しない。
