# Terminal engine sidecar

[한국어](README.ko.md)

`@soksak/sidecar-vt-alacritty`: the crates `vt-core` (PTY sessions, shell integration, inline images) and `vt-alacritty` (the terminal engine and the persistent service). The protocol and the host contract are defined in the soksak core specification (`docs/spec/sidecars.md`).

```sh
make test                 # tests
make build                # the executable that sidecar.json names
make release OUT=<folder> SOK=<core>/target/debug/sok   # the release asset for this platform
```

The checklist is [docs/features.md](docs/features.md).
