# Terminal engine sidecar

[한국어](README.ko.md)

`@soksak/sidecar-vt-alacritty`: the crates `vt-core` (PTY sessions, shell integration, inline images) and `vt-alacritty` (the terminal engine and the persistent service). The protocol and the host contract are defined in the soksak core specification (`docs/spec/sidecars.md`).

```sh
make test                 # tests
make build                # the executable that sidecar.json names
make release OUT=<folder> SOK=<core>/target/debug/sok   # the release for this platform
```

The service starts with `soksak-vt-alacritty --service-dir <dir> --log-dir <dir>`; both options are required. It writes its events to `<log-dir>/vt-<boot>.jsonl`, and [events.json](vt-alacritty/events.json) declares them. The release also carries `soksak-vt-replay`, the replay executable that `sidecar.json` declares: it rebuilds the screens of a session from the stored inputs and prints the screen hash of each frame record.

The checklist is [docs/features.md](docs/features.md).
