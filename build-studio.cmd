@echo off
setlocal EnableExtensions DisableDelayedExpansion
pushd "%~dp0"
if errorlevel 1 (
  echo Cannot open the Spectrum Studio folder.
  pause
  exit /b 1
)
set "STUDIO_CARGO=%USERPROFILE%\.cargo\bin\cargo.exe"
if exist "%STUDIO_CARGO%" goto cargo_ready
where cargo.exe >nul 2>nul
if errorlevel 1 (
  echo Rust was not found.
  echo Install Rust and the MSVC C++ Build Tools before building.
  popd
  pause
  exit /b 1
)
set "STUDIO_CARGO=cargo.exe"
:cargo_ready
echo Building Spectrum Studio...
"%STUDIO_CARGO%" build --release --manifest-path "%~dp0native\Cargo.toml" --target-dir "%~dp0native\target"
set "STUDIO_EXIT=%ERRORLEVEL%"
if "%STUDIO_EXIT%"=="0" echo Build complete. Run start-studio.cmd to launch.
popd
if not "%STUDIO_EXIT%"=="0" pause
exit /b %STUDIO_EXIT%
