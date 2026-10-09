# 변경 기록

[English](CHANGELOG.md)

## 미배포

- S33: service의 표준 오류는 `<time> <level> sidecar <where>: <text>` 형식의 글 기록을 담고, 실패한 client 인증은 오류 기록이다.
- S32: PTY에 쓴 모든 바이트와 읽은 모든 바이트가 세션, 길이, 글, 16진수 바이트를 담은 trace event이고, event `request`는 요청 본문 전체를 담으며, event `session_open`, `pty_eof`, `pty_read_error`, `session_exit`는 세션의 수명을 기록하고, 여러 thread의 기록이 섞이지 않도록 기록을 한 번의 write로 쓴다.
- S31: performance trace가 event마다 service 폴더의 플래그를 읽는다. 그래서 trace가 꺼져 있을 때 맺은 연결도 host가 trace를 켠 순간부터 request와 frame event를 쓴다.
- S30: transfer image 검사는 지켜보는 IOSurface에 표시를 붙이고 그 표시로 판정한다. IOSurface id는 전역이라, 병렬 test가 놓인 image의 id를 받을 수 있기 때문이다. `make repeat PACKAGE=<package> COUNT=<n> [TEST=<name>]`이 test를 반복하고 실행마다 보고하며, `make test`가 rustfmt를 검사한다.
- S29: service의 `hello` 응답이 이 sidecar의 version인 `version`을 싣는다. 그래서 host는 실행 중인 다른 version의 service를 설치된 것과 구별한다.
- S28: 문서와 주석이 plugin, sidecar, release를 그 말로 부른다.
