@echo off
rem Double-click to send this project to
rem https://github.com/lawindismail/Outlook-Public-Folders-Application-
rem (Git may open a browser window to sign in to GitHub the first time.)
setlocal
cd /d "%~dp0"
echo Sending the project to GitHub...
echo.
git push -u origin main
if errorlevel 1 (
  echo.
  echo The push didn't work. If GitHub asked you to sign in, sign in and run this again.
) else (
  echo.
  echo Done. Open https://github.com/lawindismail/Outlook-Public-Folders-Application-
)
echo.
pause
