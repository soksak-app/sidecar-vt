# 터미널 사이드카 (vt-core)

[English](vt-core.md)


터미널 사이드카(`@soksak/sidecar-vt-core`)는 [영속 서비스](https://github.com/soksak-app/core/blob/main/docs/spec/persistent-services.ko.md)이 정의한 영속 서비스의 클라이언트 모듈이다. 서비스는 애플리케이션 설정 디렉터리마다 하나의 프로세스로 실행되며 독립 PTY·셸 프로세스·VT 상태·스크롤백·IOSurface 렌더링을 소유한다. 사이드카는 터미널 세션 제어 요청을 받고 영역에 화면 그림을 공급한다.

| 요청 | 본문 | 의미 |
| --- | --- | --- |
| `open` | `{image?: 이름}` | 영속 서비스에서 터미널 세션을 만들거나 연결하고 호스트의 그림 `configure`를 받은 뒤 영역용 그림을 할당하고 그린다. 같은 생성 식별자에 대한 `open` 호출이 여러 번이면 아무것도 하지 않는다. |
| `input` | `{bytes?: base64-문자열 \| keys?: [{key: 이름, text?: 문자열, shift: bool, alt: bool, ctrl: bool}]}` | 터미널에 입력을 보낸다. 바이트는 base64 인코딩된 원시 터미널 입력이다. 키는 모드에 따라 터미널 수열로 디코드된다: 기능 키는 escape 수열로 매핑되고, 텍스트 입력은 UTF-8로 보내지고, 조합 키는 적절히 처리된다. `bytes`와 `keys` 모두 한 요청에 있을 수 있다. |
| `theme` | `{mode: "dark" \| "light", background, foreground, cursor, selection}` | 연결된 이미지 표면에 유효한 애플리케이션 외관을 적용한다. 네 색은 모두 필요한 `#rrggbb` 문자열이며 기본 배경과 전경, 커서 색, 선택 배경을 정한다. `mode`는 인덱스 ANSI 팔레트를 고른다. 세션과 셀 메트릭은 바꾸지 않는다. 다른 모드, 빠진 색, 다른 형식의 색은 `invalidParams`를 돌려주고 아무것도 바꾸지 않는다. 답은 적용한 배경을 담은 `{ack: true, event: "theme", mode, background}`다. |
| `cursor` | `{shape, blink, interval, idleTimeout, unfocused}` | 터미널 커서 정책을 적용한다. `shape`는 `block`·`underline`·`beam`, `blink`는 `Never`·`Off`·`On`·`Always`, `interval`은 양의 밀리초 정수, `idleTimeout`은 0 이상 밀리초 정수, `unfocused`는 `hollow`·`solid`·`underline`·`beam`·`unchanged`다. 잘못된 값은 `invalidParams`로 명시적으로 실패하며 대체하지 않는다. |
| `font` | `{family: string, size: number}` | `;`로 이은 우선순위 family 목록에서 터미널 글꼴을 선택하고 크기를 포인트(4–128, [글자 크기](https://github.com/soksak-app/core/blob/main/docs/spec/text-size.ko.md)가 13 × 실제 배율로 정함)로 정한다. 서비스는 설치된 첫 family를 적용하며, 설치되어 있지 않은 family는 건너뛰고 로그에 남길 뿐 오류로 보고하지 않는다. 목록의 family가 하나도 설치되어 있지 않으면 시스템 고정폭 글꼴을 쓰고 그 사실을 로그에 남긴다. family가 하나도 없는 목록은 `{error: "invalidParams", reason, operation: "font"}`로 거부한다. 글꼴을 선택하면 셀 메트릭, 터미널 열과 행, 세션 크기, 표시 raster를 다시 계산하고, 적용한 family와 시스템 고정폭 글꼴 여부, 건너뛴 family, 크기를 담은 `{ack: true, event: "font", family, system, skipped, size}`로 답한다. 크기가 없거나 범위를 벗어나면 빈 family 목록처럼 거부한다. 첫 `font` 요청 전에는 13포인트 시스템 고정폭 글꼴을 쓴다. 앱은 글꼴을 포함하지 않으며, 터미널 페이지는 적용한 family를 `terminal.session`의 `font`와 `fontSystem`으로 보고한다. |
| `screen.read` | `{}` | 현재 화면 상태를 요청한다. 사이드카가 [영속 서비스](https://github.com/soksak-app/core/blob/main/docs/spec/persistent-services.ko.md#화면-메시지)에 정의된 `screen` 메시지로 응답한다. |
| `close` | `{}` | 터미널 세션을 종료하고 PTY를 종료한다. 호스트의 `{closed: true}` 봉투도 같은 close 동작을 사용하므로 표면을 제거해 PTY 세션을 남길 수 없다. 서비스는 세션의 프로세스 그룹을 끝낸다. 프로세스가 모두 끝났거나 끝나는 중인 그룹은(macOS가 `EPERM`으로 답한다. 읽히지 않은 출력을 남긴 셸은 터미널 출력이 비워진 뒤에야 끝난다) 이미 닫힌 것이고, 그 밖의 실패는 그룹과 남은 프로세스를 적은 오류다. |

사이드카는 터미널 화면이 바뀔 때마다 `{event: "screen", ...}`을 보내며, 그 `frame`은 그 화면을 보이는 frame의 `{generation, raster, sequence}`(보낸 frame의 답을 기다리는 동안 바뀐 것이 있으면 다음 frame)이거나 surface에 image가 없는 동안 null이다. 그리고 새 프레임이 그려질 때마다 호스트의 그림 릴레이를 통해 그림 봉투를 보낸다. 호스트 `configure`가 페이지 주도 `resize` 요청을 대체하며 최신 래스터 리비전으로 합쳐진다. 전송 그림이 `consumed`나 오류를 기다리는 동안 사이드카는 그 그림을 수정하지 않는다. 화면과 설정 변경은 대기 상태로 남고, 응답 뒤 다음 프레임이 최신 설정을 사용한다.
