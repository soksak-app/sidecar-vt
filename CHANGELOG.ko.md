# 변경 기록

- S137: OSC 7은 `file` 스킴만 받아들인다. `kitty-shell-cwd` URI는 OSC 7 오류로 거부되며, 이 service의 shell integration은 `file`로 보고한다.
- S135: history 상한에서도 inline image가 출력과 함께 움직인다. scroll generation이 상한을 넘어 버린 줄을 세며, 일반 출력에서도 그렇다.
- S132: 세션을 닫으면 child에 먼저 SIGHUP을 보내고 최대 250 ms 뒤 SIGKILL을 보내므로, prompt에 있는 shell은 자기 job을 끝낸다.
- S131-6: input 요청은 PTY 출력을 소비하지 않으므로 출력 flood 중에도 끝난다.
- S131-5: close 표의 각 행에 test가 있고, reaper의 보고가 동시에 준비되어도 요청된 close가 이유를 정한다.
- S131-4: child가 끝나면 마지막 출력을 `Exit` 앞에 보내고, queue의 입력을 `child exited`로 실패시키고, master를 닫고, frame clock이 쥔 화면을 그린다.
- S131-3: queue의 입력 상한은 64 MiB다. 넘는 사람의 입력은 명시적 오류로 거부되고, 엔진 응답은 읽기를 멈추고 `input_held`를 보고한다.
- S131-2: I/O task 하나가 세션을 읽고 쓴다. 읽기는 64 KiB 16 chunk에서 멈추고, write는 surface task에서 기다리지 않으며, surface는 write 결과로 acknowledgement를 보낸다.
- S131-1: 세션은 ring과 attach 경로를 쥐지 않아 소비된 출력의 복사본을 남기지 않는다. persistent가 아닌 연결은 끝날 때 자기 세션을 닫는다.
- S130: 0.0.23의 frame 비용을 쟀다. status line은 frame당 1.7 ms, 20행은 frame당 3.0 ms다.
- S129: S122부터 S128까지 담은 version 0.0.23.
- S110: 서비스 start의 status 5는 process identity library의 readback 결과이며, pin된 library 0.0.25는 찾은 값을 적는다.
- S128: sidecar는 `Closed:` 사유로 닫은 항목을 받아들이는 records kit 0.0.6을 쓴다.
- S127: 세션의 image surface는 바뀐 행의 strip만 다시 그린다.
- S126: 엔진은 cell마다 할당하지 않고 화면을 만든다.
- S125: service는 frame마다 screen 메시지를 보내지 않고, `screen.read`에 full form으로 답하며, frame의 커서나 scrollback이 바뀌면 `view` event를 보낸다.
- S124: event log가 저장하지 않는 record의 frame은 hash하지 않는다.
- S123: frame 시계가 잡아 둔 출력이 화면을 만들지 않음을 test가 잰다.
- S122: 출력을 33 ms frame 시계에 맞춰 그리고, 입력 뒤 200 ms 안의 출력은 바로 그리며, 시계가 잡아 둔 출력은 화면을 만들지 않는다.
- S120: S119를 담은 version 0.0.22.
- S119: host가 image region이 그리지 않는다고 알리는 동안 surface는 frame을 그리지 않고 `screen` event를 보내지 않으며, region이 다시 그리면 바뀐 화면을 한 번 그려 보낸다. 이전 generation의 drawing 상태는 무시하고, 잘못된 drawing message에는 `invalidParams`로 답한다.
- S118: S117을 담은 version 0.0.21.
- S117: 동시에 시작하는 세션은 각자의 임시 파일로 shell integration 파일을 쓰므로, terminal 여러 개를 한 번에 열어도 `install shell integration file ...: No such file or directory`로 실패하지 않는다. Red: 동시 설치 test가 그 오류로 실패했다. Green: 통과한다.
- S115: S114를 담은 version 0.0.20.
- S116: diagnostic package test는 각 screen message를 앞 message의 화면에 적용하므로, `cells` 없는 변경 형식이 더 이상 candidate release를 실패시키지 않는다. Red: candidate release가 `screen cells are required`로 실패했다. Green: 통과한다.
- S114: 모든 `screen` 메시지는 `number`를 담고, resize 뒤, 재연결 뒤, 보내지 못한 메시지 뒤, `screen.read`, 행의 절반 넘게 바뀐 경우의 전체 형식을 빼면 그것이 가리키는 보낸 메시지 뒤에 바뀐 행만 담는다(`base: false`, `previous`, `changed`). 줄은 칸을 복사하지 않고 가진 행에서 쓴다.
- F355-4-vt: 0.0.18판. `make release OUT=<folder>`는 실행 비트를 보존하는 `vt-alacritty`의 `npm pack`으로 release를 쓰고, release.yml은 더 이상 core release의 sok를 내려받지 않는다. core가 sidecar의 `npm pack` 파일을 설치하고 `sok sidecar release`가 없기 때문이다. core 의존은 core release `v0.0.25`에 pin된다.
- F272-vt-sidecar: `main`에 push하면 tag가 없을 때 `soksak-release-gate`로 선언된 version을 core release `v0.0.21`의 `sok`와 함께 release한다. core에 대한 의존은 그 release의 pack된 release 파일이다. 어떤 workflow도 core를 checkout하지 않는다.
- S111-1: 서비스는 도착한 지 10초 뒤에도 진행 중인 요청을 operation, surface, 기다리는 단계와 함께 오류 event `sidecar.vt.request_waiting`으로 기록한다. 그래서 input loop나 surface actor의 멈춤이 기다리는 요청을 밝힌다.
- S109: owner close나 retain 뒤에, session이 끝난 surface의 메시지는 stale attachment로 거부되지 않고 새 surface의 메시지로 처리된다. 연결이 그 surface의 송신기와 부착 판을 지우기 때문이다.
- S108: `scroll` operation은 선택적 boolean `shift`를 담을 수 있다. Shift를 누른 채 wheel을 쓰면 마우스 보고를 건너뛰고 viewport를 움직이거나 alternate scroll 키를 보낸다. `shift`가 없으면 Shift가 없는 scroll이므로 `shift`를 보내지 않는 page가 계속 동작하며, boolean이 아닌 `shift`는 `invalidParams`로 거부된다.
- S107: 서비스는 마우스 모드가 바뀔 때마다 `sidecar.vt.mouse_modes`를, scroll operation마다 `sidecar.vt.wheel`을 경로와 그 시점의 모드와 함께 기록하므로, wheel이 프로그램에 갔는지 viewport로 갔는지 로그에서 확인된다.
- S45: 거부 `selection presentation is still pending`은 S43 수정을 담은 v0.0.9 이후에 없다. 이 항목은 code 변경 없이 닫힌다.
- S18: `ci.yml`이 macOS runner에서 이 저장소의 테스트를 실행하고, `release.yml`이 push된 tag v0.0.3부터 v0.0.11까지 각 tag의 sidecar archive를 게시했다.
- S106: Stale attachment 또는 malformed로 거부된 detach도 비어 있지 않은 string request가 있으면 request, name, generation, error를 담은 `body.image.detached`로 답하므로, host가 10초 timeout까지 답을 기다리지 않는다.
- S105: 추적되는 source의 주석을 영어로 쓰고, `make test`는 다른 언어의 주석을 실패시킨다.
- F275-vt-sidecar: `make install`이 고정된 의존성을 설치한다. 시험, 검사, build는 더 이상 설치하지 않는다.
- F274-1-vt-sidecar: make target 시험은 중첩된 make의 directory 줄 없이 recipe를 출력한다.
- S104: recovery verifier가 `actions`와 `preedit`의 input transaction을 보낸다.
- S103: core SDK, process identity package, `CORE_COMMIT`을 보고하는 lock 복구를 받아들이는 census의 core commit에 pin한다.
- S102: `vt-core/src/pty.rs`의 poisoned lock 복구마다 호출 자리에서 `into_inner`로 guard를 꺼내므로 core census가 이를 받아들인다.
- F274-vt-sidecar: pre-push hook은 push가 게시하는 commit message만 검사한다. 시험과 검사는 CI가 실행한다.
- S101: vt-core 시험은 input transaction을 보내고, 오류 문장을 `reason`에서 읽고, marked range를 preedit 안에 두며, JSON Schema catalog를 읽으므로 `make test`가 통과한다. Production 코드는 바꾸지 않았다.

- S100: Replay 시험은 input transaction을 보내고 그려진 각 image에 generation과 raster를 담아 답하므로 service가 시험이 기다리는 frame을 그린다. 반복 target이 2회 중 2회 통과한다.

- S86-2: Terminal 0.0.21과 함께 local 설치한 diagnostic package에서 bounded event writer를 검증했다. 연속 ASCII 입력은 기존 package에서 실패하고 수정 package에서 통과하며 종료 후 승인된 기록 50개 전체의 순서가 유지된다. 실패한 package 검사는 diagnostic 증거를 보존한다. 공개 release는 포함하지 않는다.

- S87: 큐의 flush는 다음 요청을 소비하지 않고 자신의 FIFO 경계에 응답한다. 제어된 owner는 수정 전 두 번째 응답 누락으로 실패하고 수정 후 통과하며 event-contract 14개가 통과한다. Application 전달은 남아 있다.

[English](CHANGELOG.md)

## 미배포

- S113: version은 0.0.19다.
- F355-3-4-vt: 검사는 core의 plugin API 대신 records kit과 events kit release에서 오고, core checkout의 census 검사를 실행하던 `SOKSAK_EVENTS`는 지웠다. Red: `the checks come from the kit releases, and no check takes a command or the plugin API of core`가 실패했다. Green: 통과하고, workflow, events, census test가 통과한다.
- S112: `pnpm-workspace.yaml`이 core git source의 build script를 더는 허용하지 않는다. 의존이 install script가 없는 core v0.0.21의 packed release 파일이기 때문이다.
- S97.1, S75.2, S86-3: v0.0.17이 게시되었고 registry가 이를 올린다.
- S76-2: `make test`는 doc test를 실행하지 않는다. `CompositionState`의 doc 예제는 `test_composition_state_not_composing` unit test와 중복되었고, 이 unit test는 cargo-nextest로 자체 elapsed time과 60초 제한을 받아 실행되므로 doc 예제를 설명 문장으로 바꾸었다. 변경 전에는 doc test가 test별 시간 없이 끝났고, 변경 후에는 Rust test 488개가 각각 RUNNING 줄과 elapsed time이 있는 PASS 줄을 출력하며 `make test`가 exit 0으로 끝난다.

- S76-1: `make test`는 Rust test 각 결과 앞에 RUNNING 줄을 출력한다. nextest 0.9.146의 human output에는 시작 줄이 없으므로 `scripts/nextest-run.mjs`가 test마다 started event를 내는 cargo-nextest의 libtest JSON 출력을 읽는다. 변경 전에는 Rust test 결과 488개에 RUNNING 줄이 없었고, 변경 후에는 `make test`가 RUNNING 줄 488개를 출력하며 각 줄 뒤에 PASS 줄이 있고 exit 0으로 끝난다.

- S76: `make test`는 Rust test를 `.config/nextest.toml`을 쓰는 cargo-nextest 0.9.146으로 실행하며, 각 test의 결과와 elapsed time을 보고하고 60초 뒤 test를 종료한다. 전체 suite 제한은 두지 않는다. 최종 실행은 Rust test 488회 실행, node test 30개 중 29개 통과(1개 건너뜀), records 검사를 통과했다. Doc test는 여전히 `cargo test --doc`으로 실행되어 test별 시간이 없고(S76-2), nextest는 10초 안에 끝나는 test의 시작 줄을 출력하지 않는다(S76-1). CI는 `taiki-e/install-action`으로 cargo-nextest를 설치한다.

- S99: 정확한 repository identity로 pinned Darwin process-identity Git dependency의 pnpm 11.24.0 `prepare` script 실행을 허용한다. Clean-consumer owner는 approval 없이 exit 0 후 package 누락으로 실패하고, 새 isolated store에서 approval 적용 후 통과해 `prepare: make` 뒤 manifest, header, static library, pkg-config file을 확인한다. Core package 또는 release 변경은 포함하지 않는다.

- S98: plugin SDK, Darwin process-identity dependency와 release build checkout을 0.0.21을 선언한 공개 core commit으로 맞춘다. Clean pnpm 11.24.0 lock-only resolution은 두 package를 0.0.21로 기록하며, 기존 developer link 환경에서 installed-package consumer가 통과한다. Clean frozen install은 exit 0이지만 optional native package를 resolve하지 못해 (`MODULE_NOT_FOUND`) S73-1은 열린 상태로 둔다. Release는 포함하지 않는다.

- S97: AppKit `cancelOperation:`을 terminal Escape로 매핑해 확정된 Hangul과 Escape를 순서가 유지된 한 번의 write로 PTY에 보낸다. Owning service test는 결합 transaction, cancel 단독, 일반 Escape, 최종 preedit 제거, 알 수 없는 selector의 원자적 거부를 확인한다. Selector mapping owner도 통과한다. Application, activation, full suite 또는 공개 release는 포함하지 않는다.

- S93: json!을 통해 cell tree를 복제하지 않고 소유한 정규화 screen body를 surface envelope로 옮겨 serde_json으로 직렬화한다. Raster와 plain sender가 문서화한 encoder를 사용한다. 8,192-cell 회귀 검사는 수정 전 allocation 57,372회로 실패하고 수정 후 64회 상한을 통과하며 모든 값과 escape byte를 보존한다. 영향받는 owner 네 개가 모두 통과한다. Trace 없는 양 host benchmark는 전체 report를 보존하며 여전히 성능 목표에 실패한다. Diagnostic 설치를 복원하고 성능 acceptance는 열어 둔다.
- S94: Raster draw 뒤 원본 screen을 blocking message preparation으로 옮겨 task closure의 deep copy를 제거하고 render-layout copy와 screen message 내용은 유지한다. Red: owning 8,192-cell preparation case가 64회 transfer 상한에 8,260회 allocation으로 실패한다. Green: 같은 case가 2회 allocation으로 통과하며 surface, frame, dimensions와 8,192개 cell 전체를 검증한다. Preparation owner 세 개, screen-message owner 10개, frame-record coherence, image-envelope output, page screen-wire, inline-image raster owner가 통과한다. Workspace 전체 format check는 변경하지 않은 `vt-core/tests/pty/lifecycle_test.rs` formatting 차이로 실패하며 변경한 test file은 format했다. End-to-end 성능 acceptance는 미완료다.
- S95: Diagnostics library clippy 지적 네 건과 지정 lifecycle test file의 formatting을 해결한다. Checked viewport conversion을 직접 반환하고 row 초기화에 `Vec::new`를 사용하며 dispatch payload를 버리지 않고 `SendError<Dispatch>`를 boxing하고 session에 맞는 daemon error 전송을 match guard로 합친다. Diagnostics library clippy가 통과하고 closed-surface owner는 0.142초에 통과하며 변경 Rust 파일의 rustfmt도 통과한다. Full suite와 runtime 검사는 포함하지 않는다.
- S96: 소유한 normalized cell array와 frame, render, cursor 값을 `json!`으로 Value tree를 다시 만들지 않고 screen body에 직접 넣는다. 8,192-cell owning Red는 `message_cells` allocation 73,730회, 변경 전 전체 body 131,127회, 중복 복사 57,397회를 측정한다. Green은 같은 owner에서 고정 metadata allowance 128회 안으로 통과하고 모든 message field와 cell 및 기존 screen hash를 확인한다. Screen-message golden, frame-record correlation, consumer screen-wire와 diagnostics library clippy owner가 통과한다. Suite, runtime, end-to-end performance 검사는 주장하지 않는다.

- S92: 기존 event에서 image-response channel/actor queue 시간을 전체 consumed 대기와 구분한다. 처리 timestamp 하나를 사용하고 frame debug logging이 켜진 경우에만 ingress clock을 측정하며 수집하지 않은 ingress timestamp는 null로 유지한다. Field 누락 owner가 수정 전 실패하고 수정 후 통과하며 catalog owner와 양 host 로컬 geometry 사례도 통과한다. 현재 응답 160건 모두에 queue timing이 있다. Scheduling과 frame 소유권을 유지하고 양 검사 설치를 복원하며 성능 acceptance는 열어 둔다.

- S73-2: 공개 package resolution과 build file로 설치된 Darwin native process-identity package를 검증한다. 소유 consumer 검사와 macOS arm64의 표준 sidecar build가 통과한다. 로컬 override를 누락한 remote-install fixture를 대체하며 로컬 workspace/lock override는 commit하지 않고 외부 Git-ref 설치 acceptance는 S73-1에 유지한다.

- F239-77-vt-sidecar: 일치하는 설치 diagnostic cohort로 로컬 schema-3 catalog acceptance를 완료한다. 현재 SDK가 source 34개와 pending rule 0개, event 32개를 검사하며 양쪽 host 설치본의 catalog가 source와 정확히 일치하고 application log audit을 통과한다. Public publication은 별도다.

- S88-2-1: 검증된 recovery 수정을 로컬 설치 Darwin diagnostic package에 전달한다. Package owner가 metadata, engine trace와 연속 logging으로 통과하며 양쪽 application host는 retention 완료 후 shared service를 유지하고 모든 terminal child를 회수한다. 주입된 poison recovery는 source fixture가 입증한다. Public release나 IME acceptance는 포함하지 않는다.

- S88-7: 저장한 값을 재구성하지 않고 남은 PTY session 필드 guard와 poisoned child condition wait를 회수한다. 제어된 Red case 열 개가 Green이 되고 기존 Rust owner 여덟 개가 통과한다. current-core catalog/census owner 두 개가 통과하며 VT source 감사 S88은 finding, pending rule, marker 오류 0개로 완료한다. local application 전달은 남아 있다.

- S88-4-1: 남은 feature-gated PTY wait-fault fixture를 공개 module 및 동작을 유지하며 소유 test 디렉터리에 배치한다. 기존 일회성 wait-failure owner가 통과한다. test 전용 source match 두 개를 production 분류에서 분리하며 production match 20개가 남는다. census 기준이나 제외 설정은 바꾸지 않는다.

- S88-6: retained 데이터나 수신자를 교체하지 않고 PTY output과 attachment lock을 회수한다. 각 필드 회수를 session identity와 함께 한 번 기록한다. 제어된 Red case 세 개가 Green이 되고 기존 replay/buffer owner 세 개가 통과한다. source census는 29건에서 22건으로 줄며 local diagnostic 전달은 남아 있다.

- S88-5: 연결 event writer를 사용해 poisoning 후 image transfer store를 회수한다. frame 소유와 종료 상태를 유지하고 회수를 한 번 보고하며 정확한 응답으로만 release한다. 제어된 Red case 세 개가 Green이 되고 기존 connection/image owner 네 개가 통과한다. source census는 모두 PTY에 남은 finding 29개로 30개에서 줄었다. 설치된 application 전달은 남아 있다.

- S88-4: 기존 watchdog, PTY lifecycle 및 feature-gated event fault fixture를 소유 test 디렉터리에 배치한다. 실행 test 코드, feature 제공 범위, 공개 helper method와 test 이름을 유지한다. 소유 case 일곱 개가 통과한다. 기준이나 제외 설정 변경 없이 test 전용 census match 열한 개를 production 분류에서 분리하며 production lock finding 30개가 남는다. test 배치 및 분류이며 runtime 수정이 아니다.

- S88-3: production 접근 여섯 곳에서 PTY session registry를 회수하고 공유 map을 유지하며 poisoning마다 한 번 기록한다. 제어된 Red case 세 개가 Green이 되고 실제 PTY close/exit와 재연결 owner가 통과한다. unknown-session 오류는 유지한다. source census는 47건에서 41건으로 줄며 local package 전달은 남아 있다.

- S88-2: poisoning 후 engine event queue를 회수하고 FIFO event를 유지하며 내부 runtime 오류 하나를 기존 구조화 오류 writer로 보고한다. headless 동작에서도 내부 runtime 실패를 program sequence rejection과 구분한다. 제어된 owner 네 개와 기존 event owner 세 개가 통과한다. source census는 55건에서 47건으로 줄었다. 설치된 application 전달은 남아 있다.

- S88-1: 정상적인 optional caret, viewport, blank-row 및 diagnostic metadata 기본값의 이유를 기록하고 비어 있지 않은 extent maximum의 fallback을 caret에서 시작하는 fold로 바꾼다. frame owner 두 개가 통과한다. current-core census는 60건에서 55건으로 줄고 남은 항목은 모두 poisoned lock이며 catalog는 event 32개를 수용한다. conformance 감사이며 새 display 수정은 아니다.

- S91: pending-image fixture가 관계없는 stale identity 대신 실제 첫 frame에 응답하게 한다. 이전 fixture는 unchanged source에서 실패하며 정확한 frame 계약과 충돌한다. 수정 owner는 기존 image 부재 구간 전체를 관측하고 다음 frame identity를 검증해 통과하며 production 코드와 시간 제한은 바꾸지 않는다.

- S90: raster 관련 screen JSON을 runtime blocking pool에서 준비해 async worker가 image output을 처리할 수 있게 한다. 완전한 screen/frame 데이터와 FIFO 순서를 유지하며 preparation 및 task 실패를 보고한다. 제어된 Red가 Green이 되고 집중 owner 일곱 개가 통과한다. 두 설치 diagnostic host의 log 감사와 정리가 통과하지만 drag latency와 capture 수용은 미완료다. unchanged source에서 재현한 기존 stale-response fixture 실패는 S91로 추적한다.

- S89-1: 새 host 설정에 diagnostic candidate를 설치하고 완료된 supplier wait 104개 전체를 정확한 host 수신 및 응답과 연결한다. 측정 시간 대부분은 host 수신 전이며 응답 후 지연은 0.521 ms 미만이다. 두 log 감사와 process 정리는 통과하지만 두 drag 기록은 변경하지 않은 33 ms cadence 기준을 계속 초과한다. 공개 release는 포함하지 않는다.

- S89: 기존 consumed-wait event에 정확한 image name, generation, raster, sequence를 추가한다. served-protocol owner는 수정 전 name 누락으로 실패하고 수정 후 통과하며 catalog owner 다섯 개가 모두 통과한다. event는 debug gate를 유지하며 구간은 supplier actor 및 transport 작업을 포함한다. 설치된 application 전달은 남아 있다.

- S86-1: Schema-3 catalog와 bounded event writer를 현재 VT 0.0.17
  protocol source와 통합한다. 설치된 native package 0.0.21로 diagnostic
  service가 build되고 writer 14 case 및 catalog 선언/emit-name 5 case가
  통과한다. Current-core catalog 검증은 event 32개를 수락하지만 전체 census는
  S88에서 추적하는 기존 source 60곳을 보고한다. Source integration으로 설치
  package 검증이나 publication 통과를 주장하지 않는다.

- S86: 도착 메시지와 독립적으로 event-writer batch deadline을 유지한다.
  연속 non-error traffic이 더 이상 입력이 멈출 때까지 100 ms flush를 미루지
  않는다. 새 owner는 수정 전에 268 ms 연속 입력 중 저장 기록 0개를 재현하고
  수정 후 event-contract 14 case가 수락 이벤트 순서를 유지하며 통과한다.
  Integration, diagnostic package delivery 및 publication은 열어 둔다.

- F239-77-vt-sidecar: VT 0.0.17 package catalog를 schema 3 JSON Schema로
  변환하고 event와 chain 선언을 유지한다. Red: 현재 core reader가 schema-2와
  compact-field 문제를 보고했다. Green: focused catalog test와 current-core
  catalog 검사가 통과한다. Runtime acceptance에는 current schema-3 plugin
  catalog를 새로 설치해야 하며 candidate의 P47 executable과 replay bytes는
  그대로 유지한다.

- S85: S84의 current ordered input transaction을 검증하는 VT 0.0.17
  local diagnostics candidate를 준비한다. Archive smoke는 strict insert action과
  필수 최종 preedit을 보내고 correlated acknowledgement 및 일치하는
  `render.frame`을 확인한 뒤 제어된 PTY output과 순서 있는 engine-trace `feed`
  chunk에서 복원한 정확한 command bytes를 검증한다. Red: VT 0.0.16은
  `invalidParams: input requires bytes or keys field`로 응답했다. 첫 candidate
  smoke에서 PTY trace 입력이 여러 read record로 나뉠 수 있음을 확인해, 검사는
  decoded chunk를 순서대로 결합한 뒤 정확한 input을 비교한다. 최종
  `make candidate-release`는 exit 0이고 version/package 검사 2/2가 통과한다.
  Local darwin-arm64 archive SHA-256은
  `6f3b7247bd1e14cc3ce4f25717abd0f5c7ad0e69ca79b6867cf90b5e0bfa7f9c`다.
  Diagnostics feature를 강제했고 macOS 최소 버전은 14.4다. Application
  launch, native input, tag, public release, registry 변경은 실행하지 않았다.

- S84: Native input report를 action과 필수 최종 preedit snapshot을 가진 하나의
  순서 있는 VT transaction으로 적용한다. 서비스는 부작용 전에 field 전체를
  검증하고, 순서 있는 action byte를 한 번 쓰고, 최종 preedit을 한 번 적용하며,
  앞으로 도착할 echo를 기다리지 않고 이미 준비된 PTY output을 최종 frame 전에
  반영한다. Red: 변경 전 input 처리는 legacy `bytes` 또는 `keys` field가 없다는
  이유로 `one_ordered_input_transaction_writes_actions_once_and_applies_final_preedit`
  을 거부했다. 변경 전 write failure 응답은 제출한 batch가 PTY에 일부
  전달됐는지 알 수 없다는 사실도 빠뜨렸다. 변경 전 key parser는 `text`가 없는
  native non-character key action도 거부했다. Green: non-Char key는 `text` 없이
  허용하고, `Char`에는 string `text`를 요구하며, null `Char` text나 다른 key의
  text는 부작용 전에 거부한다. 순서 있는 action과 늦은 invalid action 검사도
  통과한다.
  Ready echo cursor 검사는 중간 column 12 frame 없이 column 16의 frame 하나를
  확인하고, 기존 immediate echo 검사는 여섯 glyph baseline 및 split/combined
  기준을 유지한다. 제어된 write 실패도 원래 reason 보존, 제출한 PTY write의
  진행 상태 불명 명시, 재시도 없음으로 확인한다. 여섯 focused case가 통과한다.
  Application runtime, activation, package build, release는 실행하지 않았다.

- S83: diagnostics가 포함된 VT 0.0.16 local candidate를 준비한다. 일반
  `make release`는 non-diagnostic 상태를 유지하고, `make candidate-release`는
  `FEATURES=diagnostics`를 강제해 packaged binary smoke check를 실행한다.
  Red: `SOKSAK_VT_TRACE_DIR`를 설정해도 변경 없는 0.0.15 archive는
  `{generation:1,raster:1,sequence:1}` frame과 `render:null`을 보냈다. Green:
  0.0.16 archive는 실제 draw frame의 render metadata, 제어된 PTY 출력, 그
  출력이 기록된 engine trace를 확인한다. Version 및 package smoke 검사는
  2/2 통과하고 locked Cargo metadata와 Node syntax 검사도 통과한다. Local
  darwin-arm64 archive SHA-256은
  `f022206b32e5b44f03a7f236bad5b47f26c6c94fc4144039359f6ed28491bb85`다.
  Build는 macOS 최소 버전 14.4를 사용했고 compiler warning은 없었다. GUI,
  native input, application launch, public release는 실행하지 않았다.

- S82: S81을 담은 VT 0.0.15 local candidate를 준비한다. Package
  metadata, 두 Rust crate, Cargo.lock이 0.0.15를 선언한다.
  `tests/versions.test.mjs` 1/1과 locked Cargo metadata가 통과한다.
  공식 debug `make release` target은 선언된 native process-identity
  package 0.0.21과 기존 core native pkg-config dependency를 사용해
  `soksak-sidecar-vt-alacritty-0.0.15-darwin-arm64.tar.gz`를 만들었다.
  SHA-256은 `c3532df4b23a92de04cbf9eabb72e782d743d85861901b0cfe99b7158fb735ef`다.
  Build는 선언된 macOS 최소 버전 14.4를 사용했고 compiler warning은
  없었다. Deployment pin은 유지한다. Artifact는 local에 있으며 공개
  release나 runtime 검증을 주장하지 않는다.

- S81: 유효한 preedit grapheme을 renderer copy에서 terminal 행에 wrap하고 선택 caret을 보인다. 마지막 열의 width-2 grapheme은 분리하지 않고 다음 행으로 이동하며 한 열 raster에서는 pixel을 자른다. 기존 wide cell과 overlay가 겹치면 copy에서 기존 span 전체를 지운다. 행 끝의 전체 preedit end boundary는 UTF-16 위치를 유지하고 비어 있는 다음 행 대신 마지막 점유 cell을 frame-effective caret으로 사용한다. `screen.cursor.row/col`은 hash/replay용 raw engine 값을 유지하고, 필수 `screen.cursor.caret`은 accepted frame의 같은 layout에서 온 좌표이며 accepted frame이 없거나 dirty future frame이면 null이다. Red: 변경 전 renderer는 마지막 열의 width-2 preedit을 거부했다. 초기 wrap draft에서 두 rows=1 end-boundary test는 caret이 빈 행에 놓이고 viewport가 glyph를 자르는 것을 검출했다. `screen_message_with_layout`의 항상 object caret도 headless screen test에서 실패했다. Green: frame test 31/31, screen-message test 10/10, cropped metadata 1/1, S62 controlled draw-failure recovery와 dirty-future caret assertion 1/1, `cargo fmt --all --check`가 통과한다. Wails error sample에는 trigger preedit이 없어 runtime 원인은 모른다. Application, activation, package release는 실행하지 않았다.
- S80: S79를 포함한 VT 0.0.14 local archive를 준비한다. `vt-alacritty/package.json`, 두 Cargo crate, `Cargo.lock`이 0.0.14를 선언하고 `tests/versions.test.mjs` 1/1이 통과한다. 공식 debug `make release`가 SHA-256 `c95054417477a6a80e111265b1e84d3f514b1e7128ae46a4499287b22a80feda`인 darwin-arm64 archive를 생성했다. Deployment pin은 유지한다. Artifact는 local에 있으며 공개 release하지 않았다.
- S79.1: protocol test support module의 네 `ServeOptions` 초기화에 `submission_observer: None`을 추가한다. S79가 추가한 observer field 때문에 VT core library test target이 컴파일되지 않는다. 변경 전 컴파일 회귀 transcript SHA-256은 `405440fa05301622575dfa9997186107e1da5f9b686aa3b9c055920608da5ffc`다. 영향을 받은 library test target이 `cargo test --offline -p soksak-sidecar-vt-core --lib --no-run`으로 컴파일된다. `cargo fmt --all --check`, records 검사(104개 파일), `git diff --check`가 통과한다. Production 동작은 바꾸지 않았다.
- S79: 확정 insert와 결과 preedit snapshot을 기존 input request와 surface command 하나로 적용한다. Red: 새 split-request fixture가 확정 glyph 6개와 marked glyph 2개의 상태에서 glyph 10개를 그렸다. 변경 전 combined 처리도 combined bytes write가 시작되기 전에 ink glyph 6개, empty preedit인 frame 4를 제출했고 fake echo 처리 뒤 확정 glyph 8개인 frame 5를 제출했다. 잘못된 compose와 valid bytes 조합은 오류 뒤 두 번째 acknowledgement를 반환하면서 byte를 썼고, 잘못된 bytes와 valid compose 조합은 상관관계가 없는 오류를 반환하며 preedit을 바꿨다. Green: 같은 controlled fixture가 OutputSink queue 경계에서 frame submission을 기록하고 combined frame 시도가 write 시작과 완료 뒤인지 확인한다. `Frame` pixel은 echo 처리 전 6개 glyph, 처리 후 8개 glyph를 측정하며 preedit 중복은 없다. 두 invalid-field test는 상관된 오류 하나, write 0회, preedit 불변을 확인한다. Request acknowledgement는 PTY echo와 별도로 기록한다. Protocol version, stored format, standalone input operation과 S78.1 metadata는 변경하지 않았다. Application이나 package build는 실행하지 않았다.

- S78.1: frame-bound diagnostic metadata에 실제 draw 색과 CoreText font run 수를 보고한다. Glyph metadata는 preedit overlay를 포함해 theme default와 inverse video를 적용한 effective sRGB foreground/background를 담고, 원래 PTY screen cell은 유지한다. 각 grapheme은 양의 CoreText run 수도 보고한다. Underline은 glyph foreground를 참조한다. Difference blend가 아닌 cursor는 실제 draw 색을, difference blend block cursor는 null을 보고한다. Red: 변경 전 metadata는 overlay·inverse의 effective color와 CoreText run 수를 제공하지 않았다. Green: default frame test 23/23, diagnostics frame test 26/26, screen-message test 3/3, same-frame serve contract 1/1, Menlo의 `A`와 Devanagari combining mark fixture에서 여러 CoreText run과 잉크를 포함한 grapheme bounds를 확인했다. `cargo fmt --all --check`와 104개 file records 검사가 통과한다. Local VT 0.0.13 diagnostic archive SHA-256은 `b8c47330a37b460322bf83003d7eadd6984092c0411496cb18d4ed769ba44c8b`다. 공개 release나 runtime capture는 진행하지 않았다.

- S78: diagnostic screen 메시지에 frame과 연결된 renderer geometry를 추가한다. 명시적으로 trace를 켠 diagnostic service만 같은 draw에서 나온 raster 형식·stride, grid와 cell metrics, CoreText 및 underline geometry, cursor bounds, glyph ink bounds를 보고한다. 기본 경로는 `render: null`을 보내며 셀별 bounds를 계산하지 않는다. Red: 변경 전 screen 메시지에는 필수 `render`가 없었고 frame identity가 다른 render도 허용했으며 Makefile은 Cargo feature를 전달하지 않았다. Green: 기본 frame test 23/23, diagnostics frame test 24/24, screen-message test 3/3, service frame/queue 상관관계, 비대칭 transform unit, default/diagnostics collection gate, Makefile target test 3/3, `cargo fmt --all --check`, records 검사(104개 파일)가 통과한다. `make release FEATURES=diagnostics`로 local VT 0.0.13 diagnostic archive를 만들었다. SHA-256은 `541f8c2dc283088c6a8f858882c013c90aaaf9df7a8f8cbca35e345c99520264`다. Public release와 live capture를 실행하지 않았다.

- S77.3: 새 detach request ledger를 네 protocol test fixture에서 초기화해 VT core library test target 컴파일을 복구한다. Red: 변경 전 compile이 네 initializer에서 E0063으로 실패했다. Green: protocol test 19개와 formatting checks가 통과한다.

- S77: Terminal session과 PTY를 유지하면서 현재 render owner를 해제하는 상관관계 `image.detach` 요청과 `image.detached` 결과를 추가한다. Detach는 image name과 generation으로 범위를 제한한다. Request replay는 tuple에 결합되며 새 owner가 나타나기 전까지 reconnect 뒤에도 idempotent하다. 이미 전송된 transfer는 정확한 응답 또는 연결 종료까지 불변으로 유지하고, 늦은 응답은 전체 frame identity가 일치해야 한다. Red: 변경 전 server는 detach에 응답하지 않았고, 잘못된 frame 응답이 새 pending draw를 해제했으며, reconnect 뒤 replay는 `requestMismatch`를 반환했다. Green: targeted `serve_contract` test 7개가 통과한다. Local VT 0.0.13 macOS arm64 build와 공식 archive 명령이 통과한다. Candidate archive SHA-256은 `7fb7c038dad01828f06c185d82040c5c14d3a7ff761f2593eaf9ed5d764a8a27`다. Public release는 게시하지 않았다. Event census가 `vt-alacritty/build.rs`에서 reason annotation 누락 3개를 계속 찾으며 S77.1이 이를 추적한다.

- S75.1: pnpm local link로 core process identity package 0.0.21을 연결해
  VT 0.0.13 candidate를 build하고 package한다. Sidecar deployment Git pin은
  그대로다. 공식 build와 release 명령이 통과했으며 archive는 local에만 있다.
  공개 release는 게시하지 않았다. 선언 Git pin의 fresh install을 검증하는
  S73-1은 계속 열린 상태다.

- S75: Preedit는 `text` 기준 UTF-16 범위 또는 null인 필수 `markedRange`를 가진다. Renderer는 유효한 비어 있지 않은 범위의 grapheme에만 밑줄을 긋고 `selectedRange`의 선택 동작은 독립적으로 유지한다. Protocol input boundary는 preedit를 enqueue하기 전에 잘못된 marked range를 거부해 거부된 입력이 surface state를 바꾸지 않는다. Red: 변경 전 renderer의 `preedit_underlines_only_the_marked_utf16_graphemes`와 input 경계의 `an_invalid_marked_range_is_rejected_before_preedit_changes_surface_state`가 renderer 오류 반복으로 실패했다. Green: frame test 22개, serve-event test 23개, 영향받은 serve-contract test 3개, format, package/crate version 검사, records 검사가 통과한다. VT candidate version은 0.0.13이며 transport protocol은 1을 유지하고 preedit는 다시 읽을 format을 바꾸지 않는다. pnpm이 resolve한 native process-identity package directory가 비어 `make build`의 `process-identity-check`가 `MODULE_NOT_FOUND`로 실패해 local candidate build는 대기 중이다. Release는 게시하지 않았다.
- S74: VT sidecar는 build 시 `sidecar.json.displayName`을 embed하고 host 환경 변수를 읽지 않고 등록한다. Red: 변경 전 service가 필수 환경 값을 보고하고 ready endpoint를 게시하지 못했다. Green: 변수 부재 또는 충돌 값에서 live LaunchServices identity가 embedded manifest와 일치하며, service-cleanup 10개와 log-dir 6개, format 검사, macOS arm64 build가 통과한다. Release는 게시하지 않았다.
- S73: VT service는 선언된 process display identity를 필수로 받아 runtime 초기화 전에 LaunchServices에 등록하며 executable 이름과 첫 시작 기록을 유지한다. Red: 변경 전 service에는 LaunchServices 항목이 없었고 identity 누락도 보고하지 않았다. Green: service cleanup test 8개, scoped event/records 검사, format 검사와 macOS arm64 local version 0.0.12 build가 통과한다.

- S72: pre-push 훅이 `make push-check`를 실행한다. 이것은 ci workflow와 같이 (`make build` 다음의) `make test`와 push 범위의 commit message 검사를 실행한다. Red: `tests/make-targets.test.mjs`가 없는 recipe와 없는 훅에서 실패했다. Green: 시험 2개 중 2개가 통과하고, `make push-check`가 통과한다.

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
