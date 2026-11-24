@echo off
rem Builds the optimised app and puts a shareable copy in app\dist.
rem (Releases on GitHub are built automatically: .github\workflows\release.yml.)
setlocal
cd /d "%~dp0.."
cargo build --release || exit /b 1
if not exist dist mkdir dist
copy /y "target\release\public-folders.exe" "dist\Public Folders.exe" >nul || exit /b 1
copy /y "tools\READ ME - Public Folders.txt" "dist\READ ME - Public Folders.txt" >nul || exit /b 1
echo Shareable app is in %CD%\dist
