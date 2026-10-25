AI8 Converter for macOS
=======================

Install
-------
1. Drag "AI8 Converter" onto the "Applications" folder in this window.
2. Eject the disk image.

Everything the app needs is included (also Ghostscript for EPS/PostScript
files). Nothing else has to be installed. Works on Apple Silicon and Intel
Macs running macOS 11 (Big Sur) or later.

First launch
------------
The app is not signed with an Apple Developer ID or notarized by Apple, so
macOS blocks it the first time ("cannot be opened because the developer
cannot be verified" / "Apple could not verify ...").

Open it once like this:

  * In Finder, open Applications, right-click (or Control-click)
    "AI8 Converter" and choose "Open", then click "Open" in the dialog.

  * On macOS 15 (Sequoia) and later, if there is no "Open" button: try to
    open the app once, then go to System Settings > Privacy & Security,
    scroll down and click "Open Anyway" next to the message about
    AI8 Converter, and confirm.

  * Alternatively, in Terminal:
        xattr -dr com.apple.quarantine "/Applications/AI8 Converter.app"

After that the app opens normally with a double-click.

EPS and PS files
----------------
Reading EPS and PS needs Ghostscript, which is not included:

    brew install ghostscript

or download it from https://ghostscript.com/releases/gsdnld.html . In the app,
the Ghostscript line has a "Get Ghostscript..." button and a "Re-detect"
button. PDF, AI, SVG and DXF files convert without it.

Licenses
--------
AI8 Converter is free and open source under the MIT Licence.
Copyright (c) 2026 Asbjorn Pettersen. The licence text is inside the app at
AI8 Converter.app/Contents/Resources/LICENSE.txt, and notices for the open
source libraries it is built with are in THIRD-PARTY-NOTICES.txt beside it.

Ghostscript is not included. It is separate software by Artifex Software,
Inc., licensed under the GNU Affero General Public License v3, which you
install yourself and obtain directly from Artifex. AI8 Converter runs it as a
child process and contains no Ghostscript code.

If a build does bundle it (the opt-in --with-gs packaging flag), its licence
text, exact upstream version and source download location are inside the app at
AI8 Converter.app/Contents/Resources/gs/ (COPYING, LICENSE, BUILDINFO.txt),
summarised in GHOSTSCRIPT-NOTICE.txt.
