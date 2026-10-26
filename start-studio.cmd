@echo off
setlocal EnableExtensions DisableDelayedExpansion
pushd "%~dp0"
if errorlevel 1 (
  echo Cannot open the Spectrum Studio folder.
  pause
  exit /b 1
)
set "STUDIO_EXE=%~dp0native\target\release\rtl-spectrum-studio.exe"
if not exist "%STUDIO_EXE%" (
  echo Spectrum Studio executable was not found.
  echo Run build-studio.cmd first.
  popd
  pause
  exit /b 1
)
"%STUDIO_EXE%" --project-dir "%CD%" --open-browser %*
set "STUDIO_EXIT=%ERRORLEVEL%"
popd
if not "%STUDIO_EXIT%"=="0" pause
exit /b %STUDIO_EXIT%
