# Chromium probes for `tests/chromium.rs`

`probes.js` lists pages of boxes with no text. `main.js` opens each in a hidden Electron window of
800 by 600 CSS pixels at a device pixel ratio of 1 and writes the border box of every element with
an id (and, where a page asks, the element under some points, the scroll sizes and the boxes again
after a scroll). `gen_tests.js` turns that into `../chromium.rs`.

To collect the numbers again, with Electron 39.8.10 unpacked somewhere (the profile is a throwaway
folder in the system's temporary directory):

    ELECTRON_EXE=/path/to/electron.exe PROBE_OUT=/tmp/chromium.json node run.js
    PROBE_OUT=/tmp/chromium.json node gen_tests.js

Run Electron with `show: false` windows only; `run.js` kills it by PID after 90 seconds.
