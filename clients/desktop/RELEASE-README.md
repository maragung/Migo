Migo Desktop
============

Thank you for downloading the Migo desktop client.

Running it
----------

- **Windows**: extract the archive, then double-click `migo-desktop.exe`.
  The build is not code-signed, so Windows SmartScreen may show
  "Windows protected your PC". Click _More info_ → _Run anyway_. SmartScreen
  stops flagging the file once enough people have run it and a certificate is
  on the roadmap.
- **Linux**: extract, then run `./migo-desktop`. The binary is built on
  Ubuntu 24.04 and needs **glibc 2.39 or newer** (Ubuntu 24.04+, Debian 13+,
  Fedora 40+). On older distributions the launcher reports
  `version 'GLIBC_2.39' not found` — that is the distribution's glibc being
  older than the build, not a broken download.

Where your data lives
---------------------

The client writes its settings and the encrypted key vault under your user
profile directory (on Windows `%APPDATA%`, on Linux `~/.local/share` and
`~/.config` per the XDG convention). Nothing is installed system-wide; delete
the extracted folder and those two directories to remove the app completely.

Server
------

Out of the box the client connects to the public Migo deployment. The sign-in
screen's settings let you point it at your own server instead.

Migo is private, end-to-end encrypted messaging: your keys never leave this
machine, and the server you connect to cannot read your messages.
