' Router des Pauvres - auto-start au login Windows (port 4445)
' Installer : copier ce fichier dans
'   %APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup\
Option Explicit
Dim fso, sh, exe
exe = "C:\Users\Deamon\pauvre-router\target\release\pauvre-router.exe"
Set fso = CreateObject("Scripting.FileSystemObject")
If Not fso.FileExists(exe) Then WScript.Quit 0
Set sh = CreateObject("WScript.Shell")
sh.Run """" & exe & """", 0, False
