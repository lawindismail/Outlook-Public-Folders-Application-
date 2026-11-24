@echo off
rem Stand-in for GNU dlltool: rustc asks it to turn a .def file into an import
rem library ("-d file.def -D name.dll -l out.lib"). The GNU toolchain's own
rem dlltool needs an assembler that rustup doesn't ship, so use the LLVM
rem librarian built into rust-lld instead.
setlocal
set "DEF="
set "OUT="
set "DLL="
:loop
if "%~1"=="" goto run
if "%~1"=="-d" (
  set "DEF=%~2"
  shift
  shift
  goto loop
)
if "%~1"=="-l" (
  set "OUT=%~2"
  shift
  shift
  goto loop
)
if "%~1"=="-D" (
  set "DLL=%~2"
  shift
  shift
  goto loop
)
shift
goto loop
:run
rem rustc's .def has no LIBRARY line (the DLL name comes in -D), so add it.
set "FULLDEF=%OUT%.def"
> "%FULLDEF%" echo LIBRARY "%DLL%"
type "%DEF%" >> "%FULLDEF%"
"%USERPROFILE%\.rustup\toolchains\stable-x86_64-pc-windows-gnu\lib\rustlib\x86_64-pc-windows-gnu\bin\rust-lld.exe" -flavor link /lib /machine:x64 "/def:%FULLDEF%" "/out:%OUT%"
set "RC=%ERRORLEVEL%"
del "%FULLDEF%" >nul 2>&1
exit /b %RC%
