# 터미널 engine sidecar

[English](README.md)

`@soksak/sidecar-vt-alacritty`: crate `vt-core`(PTY session, shell integration, inline image)와 `vt-alacritty`(terminal engine과 지속 service). 프로토콜과 host 계약은 soksak core spec(`docs/spec/sidecars.md`)이 정한다.

```sh
make test                 # test
make build                # sidecar.json이 가리키는 실행 파일
make release OUT=<folder> SOK=<core>/target/debug/sok   # 이 platform의 release
```

service는 `soksak-vt-alacritty --service-dir <dir> --log-dir <dir>`로 시작하며 두 option이 모두 필요하다. event는 `<log-dir>/vt-<boot>.jsonl`에 쓰고, [events.json](events.json)이 그 event를 선언한다.

Checklist는 [docs/features.md](docs/features.md)다.
