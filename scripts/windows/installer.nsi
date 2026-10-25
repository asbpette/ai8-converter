; NSIS installer for AI8 Converter (Windows x64).
;
; Built by scripts/package-windows.sh, which stages the portable package and
; then runs:
;   makensis -DVERSION=0.1.0 -DVERSION4=0.1.0.0 -DSRCDIR=<staged folder> \
;            -DLICENSEFILE=<notice+AGPL txt> -DOUTFILE=<setup.exe> installer.nsi
;
; Installs exactly the staged portable package (both exes, README.txt, the
; licence files and "Install Ghostscript.cmd") into "Program Files\AI8
; Converter" for all users, adds Start-menu and
; (optional) desktop shortcuts and registers an uninstaller in
; "Apps & features" / "Add or remove programs".
;
; Silent install: setup.exe /S [/D=C:\Path\Without\Quotes]   (must be last)
; Silent uninstall: "C:\Program Files\AI8 Converter\Uninstall.exe" /S

Unicode true
ManifestDPIAware true
SetCompressor /SOLID lzma
RequestExecutionLevel admin

!ifndef VERSION
  !error "pass -DVERSION=x.y.z"
!endif
!ifndef VERSION4
  !define VERSION4 "${VERSION}.0"
!endif
!ifndef SRCDIR
  !error "pass -DSRCDIR=<staged package folder>"
!endif
!ifndef OUTFILE
  !define OUTFILE "AI8-Converter-${VERSION}-windows-x64-setup.exe"
!endif
!ifndef LICENSEFILE
  !error "pass -DLICENSEFILE=<license text file>"
!endif

!define APPNAME   "AI8 Converter"
!define PUBLISHER "IOTEK"
!define COPYRIGHT "Copyright (c) 2026 Asbjørn Pettersen. MIT Licensed."
!define APPEXE    "AI8 Converter.exe"
!define UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}"

!include "MUI2.nsh"
!include "x64.nsh"
!include "LogicLib.nsh"
!include "FileFunc.nsh"
!include "Sections.nsh"   ; SF_SELECTED, used by .onInit

Name "${APPNAME}"
OutFile "${OUTFILE}"
InstallDir "$PROGRAMFILES64\${APPNAME}"
InstallDirRegKey HKLM "${UNINSTKEY}" "InstallLocation"
BrandingText "${APPNAME} ${VERSION}"
ShowInstDetails show
ShowUninstDetails show

; ---- Version info resource of setup.exe ----
VIProductVersion "${VERSION4}"
VIFileVersion "${VERSION4}"
VIAddVersionKey "ProductName" "${APPNAME}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "FileDescription" "${APPNAME} ${VERSION} Setup"
VIAddVersionKey "CompanyName" "${PUBLISHER}"
VIAddVersionKey "LegalCopyright" "${COPYRIGHT}"

; ---- Modern UI ----
!define MUI_ABORTWARNING
!ifdef ICONFILE
  !define MUI_ICON "${ICONFILE}"
  !define MUI_UNICON "${ICONFILE}"
!endif
!ifdef PANELBMP
  ; Branded side panel on the Welcome and Finish pages.
  !define MUI_WELCOMEFINISHPAGE_BITMAP "${PANELBMP}"
  !define MUI_UNWELCOMEFINISHPAGE_BITMAP "${PANELBMP}"
!endif
!ifdef HEADERBMP
  ; Small logo in the header of the inner pages.
  !define MUI_HEADERIMAGE
  !define MUI_HEADERIMAGE_RIGHT
  !define MUI_HEADERIMAGE_BITMAP "${HEADERBMP}"
  !define MUI_HEADERIMAGE_UNBITMAP "${HEADERBMP}"
!endif
!define MUI_COMPONENTSPAGE_NODESC
!define MUI_FINISHPAGE_NOAUTOCLOSE
; No "Run now" checkbox on purpose: the installer runs elevated, and an
; elevated app cannot receive drag-and-drop from a normal Explorer window.
; Offer the Ghostscript guide once the install is finished, rather than having
; the app or this installer run anything on the user's behalf. README.txt sits
; in the install folder for anyone who wants it.
!define MUI_FINISHPAGE_SHOWREADME "$INSTDIR\Installing Ghostscript.html"
!define MUI_FINISHPAGE_SHOWREADME_TEXT "Read how to install Ghostscript (needed for EPS and PS files)"
!define MUI_FINISHPAGE_SHOWREADME_NOTCHECKED

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${LICENSEFILE}"
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

; Explorer caches icons by file path, so after an upgrade the Start menu,
; desktop and taskbar would keep showing the previous version's icon.
; SHCNE_ASSOCCHANGED (0x08000000) makes the shell rebuild its icon cache.
Function .onInstSuccess
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
FunctionEnd
Function un.onInit
  SetRegView 64
  SetShellVarContext all
FunctionEnd

; ---- Sections ----
Section "${APPNAME} (required)" SecMain
  SectionIn RO
  SetOutPath "$INSTDIR"
  ; Older versions bundled Ghostscript in gs\. Remove it on upgrade: it is no
  ; longer shipped, and a stale copy would be found before the user's own.
  RMDir /r "$INSTDIR\gs"
  File /r "${SRCDIR}/*.*"

  WriteUninstaller "$INSTDIR\Uninstall.exe"

  CreateDirectory "$SMPROGRAMS"
  CreateShortcut "$SMPROGRAMS\${APPNAME}.lnk" "$INSTDIR\${APPEXE}" "" "$INSTDIR\${APPEXE}" 0

  WriteRegStr HKLM "${UNINSTKEY}" "DisplayName" "${APPNAME}"
  WriteRegStr HKLM "${UNINSTKEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "${UNINSTKEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKLM "${UNINSTKEY}" "DisplayIcon" "$\"$INSTDIR\${APPEXE}$\",0"
  WriteRegStr HKLM "${UNINSTKEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "${UNINSTKEY}" "UninstallString" "$\"$INSTDIR\Uninstall.exe$\""
  WriteRegStr HKLM "${UNINSTKEY}" "QuietUninstallString" "$\"$INSTDIR\Uninstall.exe$\" /S"
  WriteRegDWORD HKLM "${UNINSTKEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINSTKEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  WriteRegDWORD HKLM "${UNINSTKEY}" "EstimatedSize" "$0"
SectionEnd

; Ghostscript is separate AGPL-3.0 software by Artifex and is NOT redistributed
; here. This section runs the "Install Ghostscript.cmd" helper that SecMain just
; installed, so there is ONE implementation of "get Ghostscript" shared with the
; portable zip. The helper uses Chocolatey when present and otherwise opens
; prints Artifex's download address, so the user always receives Ghostscript from Artifex
; under the licence Artifex's own installer presents.
; Pre-selected in .onInit when no Ghostscript is detected.
Section /o "Install Ghostscript from Artifex (for EPS/PS files)" SecGs
  DetailPrint "Getting Ghostscript from Artifex..."
  ; cmd.exe needs the whole command wrapped in its own pair of quotes when the
  ; path contains spaces: cmd /c ""C:\dir with space\x.cmd" /noprompt"
  nsExec::ExecToLog '"$SYSDIR\cmd.exe" /c ""$INSTDIR\Install Ghostscript.cmd" /noprompt"'
  Pop $0
  ${If} $0 == 0
    DetailPrint "Ghostscript is ready."
  ${Else}
    ; Nothing opens a browser on its own: say what happened and leave it. The
    ; app offers the download link itself, but only while Ghostscript is
    ; actually missing.
    DetailPrint "Could not install it automatically (exit code $0)."
    DetailPrint "Install it from https://ghostscript.com/releases/gsdnld.html"
    MessageBox MB_ICONINFORMATION|MB_OK \
      "Ghostscript was not installed automatically.$\n$\n\
Install it yourself from https://ghostscript.com/releases/gsdnld.html - pick \
the 64-bit installer for Windows.$\n$\n\
You can also retry at any time with $\"Install Ghostscript.cmd$\" in the AI8 \
Converter folder. AI8 Converter notices a new install on its own." /SD IDOK
  ${EndIf}
SectionEnd

Section "Desktop shortcut" SecDesktop
  CreateShortcut "$DESKTOP\${APPNAME}.lnk" "$INSTDIR\${APPEXE}" "" "$INSTDIR\${APPEXE}" 0
SectionEnd

; ---- Init ----
Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP|MB_OK "${APPNAME} requires 64-bit Windows (x64)." /SD IDOK
    Abort
  ${EndIf}
  SetRegView 64
  SetShellVarContext all

  ; Offer the Ghostscript step by default only if nothing is installed yet.
  ; Mirrors ghostscript::standard_locations() in src/ghostscript.rs.
  ClearErrors
  FindFirst $0 $1 "$PROGRAMFILES64\gs\gs*"
  FindClose $0
  ${If} $1 == ""
    SectionGetFlags ${SecGs} $2
    IntOp $2 $2 | ${SF_SELECTED}
    SectionSetFlags ${SecGs} $2
  ${EndIf}
FunctionEnd

Section "Uninstall"
  Delete "$SMPROGRAMS\${APPNAME}.lnk"
  Delete "$DESKTOP\${APPNAME}.lnk"

  ; Only remove what the installer put there (never RMDir /r $INSTDIR, in
  ; case it was pointed at a folder with other content).
  Delete "$INSTDIR\${APPEXE}"
  Delete "$INSTDIR\ai8conv.exe"
  Delete "$INSTDIR\README.txt"
  Delete "$INSTDIR\LICENSE.txt"
  Delete "$INSTDIR\THIRD-PARTY-NOTICES.txt"
  Delete "$INSTDIR\Install Ghostscript.cmd"
  Delete "$INSTDIR\Install Ghostscript.ps1"
  Delete "$INSTDIR\Installing Ghostscript.html"
  RMDir /r "$INSTDIR\gs"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"

  DeleteRegKey HKLM "${UNINSTKEY}"
SectionEnd
