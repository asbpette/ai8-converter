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
!define MUI_FINISHPAGE_SHOWREADME "$INSTDIR\README.txt"
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
; here. This section only triggers an install from Artifex (via the Windows
; Package Manager) or opens their download page, so the user receives it
; directly from upstream, under the licence Artifex's own installer presents.
; Pre-selected in .onInit when no Ghostscript is detected.
Section /o "Install Ghostscript from Artifex (for EPS/PS files)" SecGs
  DetailPrint "Requesting Ghostscript from Artifex via Windows Package Manager..."
  nsExec::ExecToLog 'winget.exe install --exact --id ArtifexSoftware.GhostScript \
    --accept-source-agreements --accept-package-agreements --disable-interactivity'
  Pop $0
  ${If} $0 == 0
    DetailPrint "Ghostscript installed."
  ${Else}
    ; winget is a per-user MSIX app and often refuses to run from an elevated
    ; installer, so falling back to the download page is the normal path, not
    ; an edge case.
    DetailPrint "Could not install automatically (winget exit code $0)."
    DetailPrint "Opening Artifex's download page instead."
    ExecShell "open" "https://ghostscript.com/releases/gsdnld.html"
    MessageBox MB_ICONINFORMATION|MB_OK \
      "Ghostscript could not be installed automatically.$\n$\n\
Artifex's download page has been opened in your browser: download the 64-bit \
Windows installer and run it.$\n$\n\
You can also run $\"Install Ghostscript.cmd$\" from the AI8 Converter folder \
at any time. Afterwards start AI8 Converter and press $\"Re-detect$\" on the \
Ghostscript line." /SD IDOK
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
  RMDir /r "$INSTDIR\gs"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"

  DeleteRegKey HKLM "${UNINSTKEY}"
SectionEnd
