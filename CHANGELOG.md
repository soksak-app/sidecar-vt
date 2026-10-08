# Changelog

[한국어](CHANGELOG.ko.md)

## Unreleased

- S31: the performance trace reads the flag of the service directory before each event, so a connection made while the trace was off writes its request and frame events from the moment the host turns the trace on.
- S30: the transfer image checks mark the IOSurface they watch and judge it by that mark, because IOSurface ids are global and a parallel test can receive the id of a released image. `make repeat PACKAGE=<package> COUNT=<n> [TEST=<name>]` repeats tests and reports each run, and `make test` checks rustfmt.
- S29: the `hello` reply of the service carries `version`, the version of this sidecar, so a host tells a running service of another version from the installed one.
- S28: the documents and comments call plugins, sidecars and releases by those words.
