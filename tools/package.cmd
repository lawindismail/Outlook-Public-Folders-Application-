@echo off
rem Builds the optimised app and puts a shareable copy in app\dist, and the
rem downloadable copy for the GitHub page in app\download.
setlocal
cd /d "%~dp0.."
cargo build --release || exit /b 1
if not exist dist mkdir dist
if not exist download mkdir download
copy /y "target\release\public-folders.exe" "dist\Public Folders.exe" >nul || exit /b 1
copy /y "tools\READ ME - Public Folders.txt" "dist\READ ME - Public Folders.txt" >nul || exit /b 1
copy /y "target\release\public-folders.exe" "download\Public Folders.exe" >nul || exit /b 1
copy /y "tools\READ ME - Public Folders.txt" "download\READ ME - Public Folders.txt" >nul || exit /b 1
echo Shareable app is in %CD%\dist and %CD%\download
