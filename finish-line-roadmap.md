Finish live p2p sync collab, get authority and ux right, no bugs or perf issues:
Collab---discovery via DB/gdrive? Is it possible?---DB especially, since that'll be for native workflows where a file is synced via DB.
Finish flow, refactor it to work with p2p sync collab after merging that into main, hammer out UX
Flow:
Right-click = draw
freeform flow option
Tub---finish it up, make it beautiful
Add aliasing
Add toolkits
Integrate w/ collab
Within-app splitscreening
Finish keymap
Style alias enum---fill it out
Verify .docx conversion semantics---grab entire DB:
Two paths
.docx -> .cmir -> .db8
.docx -> .db8 
EVERY difference needs to be surfaced and investigated---.cmir isn't source of truth, but could tease out where we're wrong

Web---compile to gpui wasm.
Dropbox integration--this should be partially platform agnostic
Google drive integration---this should be partially platform agnostic
^ Replace filesystem on web
Iroh---can compile to wasm and work on web!

Mobile

Workers-rs---cloudfare rust workers package---for web

Verbatim feature parity
Cardmirror feature parity
https://github.com/ant981228/cardmirror/blob/main/MANUAL.md#18-whats-not-here-yet
.cmir support, input-output, round trip

Settings ui---fix

Ribbon ui---fix

Full extensibility via WASM:
Rust
Python
JS/TS
C
C++
C#
The other one?
