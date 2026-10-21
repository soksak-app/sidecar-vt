# 변경 기록

[English](CHANGELOG.md)

## 미배포

- S69: 프로세스가 어떤 setup보다 먼저 첫 동작으로 기록을 한 번 쓴다. 표준 오류에 쓰는 text record `<UTC time> info sidecar vt: started`이고, host는 그것을 layer `sidecar`의 record로 저장하므로, 프로세스의 spawn부터 첫 기록까지의 시간이 보인다. 기록은 event file이 아니라 표준 오류로 간다. contract가 정한 file의 첫 줄은 `log.open`이기 때문이다. 깨끗하게 끝나는 실행은 이제 표준 오류에 정확히 이 기록만을 쓴다.

- S68: 새 커밋의 작성자는 `min-0-max <max@blue.tools>`다. `.mailmap`이 이전 이름 `min-median-max`를 잇고, 게시된 커밋은 다시 쓰지 않는다.

- S66: persistent service는 service folder가 지워지면(`folder-removed`), `endpoint.json`이 지워지면(`endpoint-removed`), `endpoint.json`이 다른 service를 가리키면(`endpoint-replaced`) 정상 정리를 거쳐 끝나며, 끝남은 그 reason과 함께 `sidecar.vt.session_stop`으로 기록된다. service는 `endpoint.json`이 자기를 가리킬 때만 그것을 지우므로, folder를 가져간 다른 service의 endpoint는 남는다. service 검사는 자신의 folder를 삭제 실패를 보고하는 guard로 지우고, SIGKILL로 끝난 service의 socket folder를 지운다.

- S67: `vt-core/tests/event_contract.rs`의 검사는 끝날 때 자신의 folder와 그 옆의 설정 folder를 통과하든 실패하든 지우며, 지우기 실패는 `error:`로 보고한다. 전에는 어떤 검사도 자신의 folder를 지우지 않아 실행마다 임시 folder에 `vt-event-contract-*` 항목이 남았다.

- S63.3: version 0.0.11이 S63, S64와 S65를 릴리즈한다.
- S65: `vt-alacritty/tests/log_dir_test.rs`의 검사는 통과하든 실패하든 시작한 service를 끝내고 그 folder를 지우며, service나 folder가 남으면 실패한다. 전에는 실패한 검사가 parent 1인 service와 그 folder를 남겼다.
- S64: 기본 정책은 모든 class에서 `error`만 저장하며, 첫 정책 전과 `log-policy.json`이 없는 동안 적용된다. 그러므로 `warn`, `info`, `debug`, `trace` event는 거부된다. 골든 사례 `vt-core/tests/fixtures/log-contract/policy-cases.json`은 core에서 복사했다. 테스트의 writer는 모든 level을 저장하므로 error보다 낮은 event를 검사하는 테스트에서도 저장된다.
- S63.2: image와 함께 열렸거나 configure된 image를 가진 surface에 대한 `image` 없는 `open`은 `imageAttached`로 거부되고 아무것도 바꾸지 않아, surface가 session의 출력을 계속 그린다. 이전에는 그런 `open`이 surface를 headless로 만들어 Enter 뒤의 출력이 다음 key나 focus 변화가 frame을 그릴 때에만 보였다.
- S63.1: `open`과 적용된 `image.configure`마다 surface에 남긴 그리기 mode를 `sidecar.vt.surface_mode` `{mode, cause}`로 기록해, 출력을 그리지 않고 처리하는 surface가 event log에 드러난다.
- S62: draw에 실패한 화면은 unsent로 남아, `screen` event가 어떤 frame도 담지 않은 화면에 대해 마지막으로 보낸 frame 대신 다음 frame을 가리키고 다음 변경이 그 frame을 그린다.
- S59: `screen` event가 그 화면을 보이는 frame의 `generation`, `raster`, `sequence`를 `frame`으로 싣는다. 그래서 screen 읽기가 표시된 frame을 이름으로 알린다. image 상태가 없으면 `frame`은 null이다.
- S60: `make hooks`가 `.githooks`를 hook 폴더로 정해, commit message의 형식이나 용어 규칙, record 규칙을 어긴 commit이 만들어질 때 거부된다.
- S58: 막힌 쓰기의 test가 고정된 시간 대신 응답의 첫 byte를 기다린다.
- S57: 모든 crate를 Rust edition 2024로 빌드한다. manifest가 `edition = "2024"`를 선언하고 동작은 그대로 둔다. 진단 build의 engine 기록 폴더는 시작할 때 `SOKSAK_VT_TRACE_DIR`에서 한 번 읽는다.
- S56: session의 close가 writer를 가져가기 전에 자식을 끝내므로, 읽지 않는 자식을 기다리는 write가 close를 붙잡지 않는다.
- S55: session의 write와 input loop의 소유자 닫기를 runtime worker 밖에서 실행하므로, 읽지 않는 자식을 기다리는 write가 runtime의 다른 task를 막지 않는다.
- S54: service가 policy 파일을 kqueue로 폴더와 파일에서 감시하므로 log 폴더에 쓰는 동안에도 변경을 받는다. `sidecar.vt.policy.watch_started`와 `sidecar.vt.policy.watch_event`는 level info로 선언한다.
- S49: host의 답은 놓인 surface가 남긴 transfer image를 이름, generation, raster, sequence가 모두 맞을 때만 놓으므로, 같은 surface의 다음 raster의 `consumed`가 surface에 닿고 그 configure가 그린다.
- S52: service가 1 s 이상의 runtime 멈춤을 `sidecar.vt.runtime_stall`로, thread들의 stack을 `sidecar.vt.stall_dump`로, 끝을 `sidecar.vt.runtime_resumed`로 기록한다.
- S50: `make test`가 record 검사 `soksak-records`를 실행하고, CI가 push된 각 commit message의 형식을 `soksak-commits`로 검사한다.
- S40.15: `@soksak/plugin-api`를 core release 0.0.11에서 받으므로 `make test`가 설치된 `soksak-events`로 Rust source의 emit site를 catalog와 대조하고, release workflow가 그 release에서 `sok`를 build한다.
- S51: `tests/events.test.mjs`의 manifest case가 `sidecar.json`의 replay input과 그 catalog 선언을 요구한다.
- S43: 앞 release의 event가 raster를 기다리는 동안 온 release를 거부하지 않고 그 event 뒤에 적용한다.
- S41.6: `sidecar.json`이 `replay.inputs`에 재생 입력 종류마다 event를 적는다.
- S41.2: `census.json`이 Rust source의 writer 진입점을 emitter로 선언하고, `make test`가 `--catalog vt-alacritty/events.json`으로 그 site를 catalog와 대조하며, catalog는 `sidecar.vt.session_open`의 field `cwd`와 `pid`를 선언한다.
- S41.5: event `screen`이 같은 화면의 frame 기록을 담은 event 파일의 `boot`인 `segment`를 담는다.
- S41.4: event `screen`이 `ch`와 boolean attribute를 가진 cell의 `lines` 대신 core의 screen message(frame 기록의 `sequence`, `rows`, `cols`, `cursor`, cell마다 `grapheme`, `width`, 해석된 `fg`와 `bg`, `attributes`)를 담고, raster 없이 보낸 화면도 frame 기록을 저장한다.
- S40.10: `make test`와 CI가 `census.json`으로 core의 census를 실행하고, thread의 panic은 오류 기록이며, 모든 이유 주석은 `default:` 또는 `silent:` 형태이고, catalog는 chain `input`을 `pty_write`까지 선언한다.
- S40.13: service는 `sidecar.json`에 `replay`로 선언한 replay 실행 파일 `soksak-vt-replay`를 낸다. 이 파일은 저장한 입력에서 session의 화면을 다시 만들고 frame 기록마다 hash를 출력한다.
- S40.12: session의 입력은 replay 입력(`sequence`와 `hex`를 가지고 text 사본이 없는 `pty_read`, `pty_write`, `resize`)이고 event `frame`은 `sequence`, `rows`, `cols`, 화면 `hash`를 가진 frame 기록이다.
- S40.11: `screen_hash`가 core의 화면 hash를 계산하고 golden vector를 재현한다.
- S40.7: 칸이 없는 raster, integration이 없는 shell, 적용할 수 없는 inline image 명령을 기록한다.
- S40.9: test가 event `consumed_wait`, `read_wait`, `output_blocked`를 일으키고 그 필드를 확인한다.
- S40.8: event `frame`은 모든 frame마다 표면, session, `sequence`, `size`, `raster`와 함께 저장한다. 화면의 hash는 core의 정의를 기다린다.
- S40.6: service의 중지와 끝(signal 또는 shutdown 요청을 담은 `session_stop`, `session_end`), 끊긴 client, session의 kill(ESRCH를 포함해 신호가 닿은 결과를 담은 `kill`), 출력 ring에서 나가는 chunk와 broadcast가 제거하는 attachment(`buffer_dropped`)는 event이고, panic한 client task는 오류 기록이다.
- S40.3: 응답 줄은 `surface`, `body` 옆에 요청의 `cids`를 싣고, 각 응답은 `ok`, `micros`, `<code>: <text>` 형태의 `error`를 가진 event `sidecar.vt.reply`다. headless session을 열지 못한 실패는 표면 이름을 가진다.
- S40.4: 요청 event는 요청 전체를 복제하거나 직렬화하지 않는다. 줄의 글을 빌려 쓰고 1,000,000 바이트를 넘는 줄은 head와 크기로 기록하며, writer는 1,048,576 바이트를 넘는 fields를 `truncated`와 함께 head로 저장한다.
- S40.5: `pty_write`와 `pty_read`의 hex는 lookup table로 만든다(8192 바이트 chunk에서 이전 할당 8211회, 지금은 32회 이하). `pty_read`는 policy가 class `io`를 저장할 때만 만들어 queue에 넣고, `log.open`이 policy를 적는다.
- S40.2: event writer의 실패는 event다. 거절 이유마다 별도의 `log.dropped`가 있고, 실패한 write는 그 줄을 잃은 뒤 다음 write가 성공하면 `log.dropped`로 기록하며, 실패한 rotation 단계와 늦은 flush 답은 위치 `event writer`의 오류 기록이다.
- S40.1: event 파일은 schema 2다. `log.open`이 `version`과 `policy`를 담고, `events::read_lines`는 다른 schema의 파일을 파일 이름과 찾은 번호를 담은 오류로 거부하며, golden fixture는 core의 것이다.
- S39: `sidecar.json`이 필드 `events`로 event catalog `events.json`을 가리키고, catalog는 패키지 `vt-alacritty` 안으로 옮겼으며, 패키지의 `files`가 그것을 담아 릴리스가 catalog를 나른다.
- S38: `vt-alacritty/events.json`이 service의 event 15개(layer, class, level, 필수 필드와 그 type, 설명)를 선언하고, `tests/events.test.mjs`와 `vt-core/tests/serve_events.rs`가 선언한 event마다 이를 이름 붙인 source가 있는지, source가 이름 붙인 event가 모두 선언되었는지, 실제 session의 event가 catalog를 만족하는지 검사한다.
- S37: service의 기록은 필드 `where`와 `text`를 가진 event `sidecar.vt.record.error`와 `sidecar.vt.record.info`이고, 표준 오류에는 process나 system이 쓰는 것만 남는다. `service_log`는 없앴고, 실패한 client 인증, 실패한 정리, 설치되지 않은 글꼴 family, 쓸 수 없는 trace 폴더는 event 파일의 기록이다.
- S36: performance trace의 event는 event 파일의 event `sidecar.vt.pty_write`, `pty_read`, `pty_eof`, `pty_read_error`, `session_open`, `session_exit`, `frame`, `consumed_wait`, `read_wait`, `output_blocked`, `request`, `session_start`, `client_connect`이고 항상 저장한다. 수준은 `pty_read`, `frame`, `consumed_wait`가 `debug`이고 나머지는 `info`다. 요청 줄의 `cids` 필드(문자열 목록이며 다른 값은 parse 오류로 답한다)는 event `request`와 `pty_write`의 줄 최상위에 쓰이고, `SessionPort::write`와 `PtyService::write`가 이를 받는다. 플래그 파일 `performance`와 모듈 `performance`는 없앴다.
- S35: service는 event를 event contract의 JSON line으로 `<log-dir>/vt-<boot>.jsonl`에 쓴다(폴더 mode 0700 안의 mode 0600 파일, 첫 줄 `log.open`, `seq`는 1부터, 100 MB에서 rotation하고 이전 파일 5개를 유지, 거절한 event는 `log.dropped`). 디렉터리는 새 필수 인자 `--log-dir <dir>`에서 받는다. 이 인자가 없으면 service는 시작을 거부하고, 값이 없는 인자, 반복한 인자, 모르는 인자는 오류다.
- S33: service의 표준 오류는 `<time> <level> sidecar <where>: <text>` 형식의 글 기록을 담고, 실패한 client 인증은 오류 기록이다.
- S32: PTY에 쓴 모든 바이트와 읽은 모든 바이트가 세션, 길이, 글, 16진수 바이트를 담은 trace event이고, event `request`는 요청 본문 전체를 담으며, event `session_open`, `pty_eof`, `pty_read_error`, `session_exit`는 세션의 수명을 기록하고, 여러 thread의 기록이 섞이지 않도록 기록을 한 번의 write로 쓴다.
- S31: performance trace가 event마다 service 폴더의 플래그를 읽는다. 그래서 trace가 꺼져 있을 때 맺은 연결도 host가 trace를 켠 순간부터 request와 frame event를 쓴다.
- S30: transfer image 검사는 지켜보는 IOSurface에 표시를 붙이고 그 표시로 판정한다. IOSurface id는 전역이라, 병렬 test가 놓인 image의 id를 받을 수 있기 때문이다. `make repeat PACKAGE=<package> COUNT=<n> [TEST=<name>]`이 test를 반복하고 실행마다 보고하며, `make test`가 rustfmt를 검사한다.
- S29: service의 `hello` 응답이 이 sidecar의 version인 `version`을 싣는다. 그래서 host는 실행 중인 다른 version의 service를 설치된 것과 구별한다.
- S28: 문서와 주석이 plugin, sidecar, release를 그 말로 부른다.
