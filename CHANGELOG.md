# Changelog

[한국어](CHANGELOG.ko.md)

## Unreleased

- S35: the service writes its events as JSON lines of the event contract to `<log-dir>/vt-<boot>.jsonl` (mode 0600 in a folder of mode 0700, first line `log.open`, `seq` from 1, rotation at 100 MB with 5 earlier files, `log.dropped` for a refused event). The directory comes from the new required argument `--log-dir <dir>`; the service refuses to start without it, and an argument without a value, a repeated argument and an unknown argument are errors.
- S33: the standard error of the service holds text records of the form `<time> <level> sidecar <where>: <text>`, and a failed client authentication is an error record.
- S32: every write to and read from the PTY is a trace event with its session, length, text and bytes in hexadecimal, the event `request` carries the whole request body, the events `session_open`, `pty_eof`, `pty_read_error` and `session_exit` record the life of a session, and a record is written with one write so that records of several threads are not mixed.
- S31: the performance trace reads the flag of the service directory before each event, so a connection made while the trace was off writes its request and frame events from the moment the host turns the trace on.
- S30: the transfer image checks mark the IOSurface they watch and judge it by that mark, because IOSurface ids are global and a parallel test can receive the id of a released image. `make repeat PACKAGE=<package> COUNT=<n> [TEST=<name>]` repeats tests and reports each run, and `make test` checks rustfmt.
- S29: the `hello` reply of the service carries `version`, the version of this sidecar, so a host tells a running service of another version from the installed one.
- S28: the documents and comments call plugins, sidecars and releases by those words.
