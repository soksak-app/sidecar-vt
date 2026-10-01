# 터미널 engine sidecar

[English](README.md)

`@soksak/sidecar-vt-alacritty`: crate `vt-core`(PTY session, shell integration, inline image)와 `vt-alacritty`(terminal engine과 지속 service). 프로토콜과 host 계약은 soksak core spec(`docs/spec/sidecars.md`)이 정한다.

```sh
make test                 # test
make build                # sidecar.json이 가리키는 실행 파일
make release OUT=<folder> SOK=<core>/target/debug/sok   # 이 platform의 release asset
```

Checklist는 [docs/features.md](docs/features.md)다.
