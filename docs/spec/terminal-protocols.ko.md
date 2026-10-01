# 터미널 프로토콜 지원 범위

[English](terminal-protocols.md)

이 목록은 터미널 런타임 (soksak core `docs/spec/terminal-runtime.ko.md`)에 속한다. 제어 시퀀스를 파싱하는 것만으로 기능이 구현되지 않는다. 요청한 효과나 응답, 정책이 금지하면 관측 가능한 거부, 회귀검증 근거가 있어야 완료다. 이 조건을 기록하기 전에는 지원 범위가 미완료다.

## 기준 자료

기준 문서는 [XTerm 제어 시퀀스 patch 411, 2026-08-23](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html)이다. OSC는 제어 문자열의 종류이며 벤더 기능 전체가 보편적이고 닫힌 하나의 OSC 기능 집합은 아니다. 고정한 문서의 모든 선택자를 목록에 넣고 벤더 확장은 별도로 기록한다. 일부 지원을 전체 OSC 지원으로 설명하지 않는다.

커서 설정은 다음 개념을 다룬다. 블록, 밑줄, 막대, 깜박임 정책, 간격, 유휴 제한, 비활성 외곽선이다. 프로그램 커서 제어는 OSC가 아닌 CSI 동작이다.

OSC 1337 이미지 전송은 OSC 확장을 사용한다. APC 그래픽 프로토콜은 APC를 사용한다. 클립보드 이미지 붙여넣기, 이 프로토콜들, 파일 드래그앤드롭은 각각 요구사항이 있으며 하나를 구현해도 나머지를 구현한 것은 아니다.

## 필요한 근거

| 영역 | 요구 동작 | 현재 근거 |
| --- | --- | --- |
| 제목·아이콘 메타데이터 | 내용과 순서를 보존하고 소유 터미널만 갱신 | 엔진 이벤트 추가. 전체 UI 검증 대기 |
| 인덱스·동적 색상 | 팔레트 설정·조회·초기화, 전경·배경·커서 효과와 정확한 응답 | 인덱스 색상 셀 변환 검사. 전체 OSC 검증 대기 |
| 하이퍼링크 | 줄바꿈 셀과 선택에서 링크 식별 유지. 사용자 명령으로만 열기 | 대기 |
| 클립보드 선택 | 텍스트 전체 보존, 조회 응답 연결, 사용자 붙여넣기와 프로그램 요청 구분 | 엔진 이벤트 추가. 네이티브·권한 통합 대기 |
| 디렉터리·셸 메타데이터 | 내용을 실행하지 않고 문법 검증. 소유 세션 갱신 | typed `directory`·vendor namespace `vendor.shell.state` event와 parser·소유 표면 검사가 통과함 |
| 알림 | 소유 세션을 표시하고 내용을 실행하지 않기 | typed `notification` event와 parser·잘못된 입력·소유 표면 검사가 통과함 |
| 폰트·로깅·창·리소스 동작 | 명세 의미 구현 또는 미지원·정책 거부를 명시. 성공하는 무동작 금지 | 대기 |
| 터미널 내 그래픽 | 크기·인코딩·한계·수명 검증, 격자 배치·삭제 의미 보존 | 대기 |
| 알 수 없거나 잘못된 시퀀스 | 비밀 내용을 로그에 복사하지 않는 제한된 진단 메타데이터 보고. 지원 주장 금지 | 사이드카는 엔진의 거부를 `sequence.rejected` 이벤트로 보내고, 페이지는 최근 이유 8개를 `terminal.session.rejected`에 두며, 이는 프로그램 출력에 대한 것이므로 터미널 오류나 표면 오류로 알리지 않는다. 프로토콜 창 검사가 이를 읽는다 |
| 커서 제어 | 터미널 표시·모양·깜박임 제어와 사용자 정책 준수. 실제 픽셀·위치 검증 | 엔진·렌더러 통합 중 |
| 기본 화면 줄 재배치 | 좁힘·넓힘에서 자동 줄바꿈 논리행과 명시적 개행 보존 | 엔진 검사 존재. 현재 재빌드 호스트 검사 대기 |

이 표는 요구사항 목록이며 선택자별 적합성 검증 완료 보고가 아니다. 전체 OSC 지원 주장의 근거가 될 수 없다. 해당하는 각 행을 선택자 단위 검사로 확장해야 완료로 표시할 수 있다.

## OSC 선택자 inventory

다음은 고정한 XTerm 기준과 현재 Alacritty handler를 selector 단위로 대조한 결과다. `implemented`는 엔진이 관측 가능한 효과나 응답을 낸다는 뜻이다. `unsupported`는 이 런타임이 효과·응답을 의도적으로 제공하지 않는다는 뜻이며 지원한다고 설명해서는 안 된다. `vendor`는 별도 계약으로 추적하며 표준 OSC 완료 범위에 넣지 않는다. `scripts/check-terminal-protocol-inventory.mjs`는 이 표를 엔진 목록과 선택자 하나씩 비교한다. 엔진 선택자에 이 표의 행이 없거나, 행에 엔진 선택자가 없거나, 결과가 다르거나, 행이 테스트를 적지 않거나, 적은 테스트가 사이드카 소스에 없으면 실패한다.

| 선택자 | XTerm 동작 | 현재 결과 | 이름 있는 근거 |
| --- | --- | --- | --- |
| `0`, `2` | 아이콘 이름 및 창 제목 설정 | `implemented`: 제목 event. 아이콘 소유권은 노출하지 않음 | `vt_events_are_retained_and_exposed_in_order`, `osc_title_supports_bel_st_and_fragmentation` |
| `1`, `3` | 아이콘 전용 제목 / X property | `unsupported`: 아이콘·X property 호스트 계약 없음 | `osc_selector_inventory_records_unsupported_operations` |
| `4` | 인덱스 색상 설정·조회 | `implemented`: 팔레트 효과와 정확한 RGB 응답 | `indexed_colors_and_combining_characters_survive_export`, `every_default_indexed_color_query_returns_the_default_palette` |
| `5`, `6` | 특수 색상 설정·조회 / 활성 상태 | `implemented`: OSC 6이 켠 동안 색 0(굵게), 1(밑줄), 3(반전), 4(기울임)가 그 속성 글자의 기본 전경색을 대신한다. 색 2(깜빡임)는 격자가 깜빡임 속성을 보관하지 않으므로 명시적 오류다 | `osc_special_colors_draw_attributed_text_when_enabled_and_answer_queries` |
| `10`–`12` | VT 전경·배경·커서 색상 | `implemented`: 효과와 조회 응답 | `dynamic_color_replies_and_screen_colors_use_the_same_palette`, `osc_default_color_queries_match_renderer_defaults` |
| `13`, `14` | 포인터 전경·배경 색 | `unsupported`: 페이지는 시스템이 정해진 색으로 그리는 macOS 시스템 포인터 그림을 보이므로, 터미널은 포인터 색을 바꿀 수 없다 | `osc_selector_inventory_records_unsupported_operations` |
| `15`, `16`, `18`, `21` | Tektronix 색과 제목 | `unsupported`: 터미널에 Tektronix 에뮬레이션이 없다 | `osc_selector_inventory_records_unsupported_operations` |
| `17`, `19` | 강조 배경·글자 색 | `implemented`: 설정, 요청과 같은 종결자의 조회 응답, 선택 칸을 반전 대신 그 색으로 그림. 설정하지 않은 색은 반전 색으로 답한다 | `osc_highlight_colors_are_set_queried_reset_and_draw_the_selection` |
| `22` | 포인터 모양 | `implemented`: X 커서 글꼴 이름과 CSS 이름을 터미널 뷰의 CSS cursor로 바꾸고 `terminal.session.pointer`로 알린다. 빈 이름은 기본값으로 되돌리고, 모르는 이름은 명시적 오류다 | `osc22_sets_the_pointer_shape_and_rejects_unknown_shapes` |
| `46` | 로그 파일 | `unsupported`: 터미널 process가 호스트 로그 파일을 선택하지 않음 | `osc_selector_inventory_records_unsupported_operations` |
| `50` | 커서 글꼴·모양 동작 | `implemented`: 지원하는 커서 모양 하위 형식만. 다른 글꼴 형식은 parser 계약에서 거부 | `osc50_cursor_shape_changes_program_cursor` |
| `51` | Emacs shell 예약 | `unsupported`: 효과 없음 | `osc_selector_inventory_records_unsupported_operations` |
| `52` | clipboard selection 저장·조회 | `implemented`: 정책 제한 typed event와 조회 응답 | `clipboard_query_uses_a_token_and_resolves_to_pty_bytes`, `clipboard_query_survives_a_fragmented_st_terminator`, `clipboard_rejection_clears_a_pending_query_token` |
| `60`–`62` | 권한 기능 조회 | `unsupported`: capability 상태는 XTerm wire 응답이 아닌 sidecar 계약이 소유 | `osc_selector_inventory_records_unsupported_operations` |
| `104` | 인덱스 색상 초기화 | `implemented`: 팔레트 초기화 | `osc104_resets_indexed_colors`, `osc104_without_parameters_resets_all_indexed_colors` |
| `105`, `106` | 특수 색상 초기화 / 활성 상태 | `implemented`: OSC 105는 번호의 특수 색이나 모든 특수 색을 초기화한다. OSC 106은 OSC 6과 같다 | `osc_special_colors_draw_attributed_text_when_enabled_and_answer_queries` |
| `110`–`112` | 동적 색상 초기화 | `implemented`: 전경·배경·커서 초기화 | `osc_dynamic_color_resets_restore_defaults` |
| `117`, `119` | 강조 색 초기화 | `implemented`: 초기화한 색의 선택은 다시 반전으로 그린다 | `osc_highlight_colors_are_set_queried_reset_and_draw_the_selection` |
| `I`, `l`, `L` | Sun/CDE 아이콘·제목 형식 | `unsupported`: icon-label·비숫자 선택자 계약 없음 | `osc_selector_inventory_records_unsupported_operations` |
| `7` | 현재 작업 디렉터리 URI | `vendor implemented`: typed `directory` event. 빈 URI는 거부 | `vendor_osc_effects_are_typed_and_survive_bel_st_and_fragmentation`, `malformed_vendor_osc_is_rejected_without_silent_drop`, `vendor_event_is_emitted_only_with_the_owning_surface_id` |
| `8` | hyperlink parameter·URI | `vendor implemented`: typed open/close event. 알 수 없거나 중복된 parameter는 거부 | `vendor_osc_effects_are_typed_and_survive_bel_st_and_fragmentation`, `malformed_vendor_osc_is_rejected_without_silent_drop` |
| `9` | notification message | `vendor implemented`: typed notification event. 빈 message는 거부 | `vendor_osc_effects_are_typed_and_survive_bel_st_and_fragmentation`, `malformed_vendor_osc_is_rejected_without_silent_drop` |
| `133` | shell integration prompt·command marker | `vendor implemented`: typed marker·parameter. 알 수 없는 marker는 거부 | `vendor_osc_effects_are_typed_and_survive_bel_st_and_fragmentation`, `malformed_vendor_osc_is_rejected_without_silent_drop` |
| `1337` | OSC 1337 inline image | `vendor implemented`: 제한된 typed image·multipart 소유권 계약 | `osc1337_inline_image_is_typed_and_survives_input_chunk_boundaries`, `test_inline_image_event_is_explicit_and_base64_encoded`, `test_inline_image_delete_is_explicit_for_unowned_names` |

unsupported 행은 성공한 무동작 결과가 아니라 명시된 범위 결과다. 엔진은 입력 청크가 나뉘고 BEL 또는 ST로 끝나는 경우를 포함해 모든 unsupported 선택자에 관측 가능한 거부 event를 낸다. inventory 검사는 중복·미분류 선택자와 이름이 맞지 않는 테스트를 실패시키며 parser가 받아들였다는 사실로 지원을 추론하지 않는다.

## Vendor 계약

선택한 vendor 범위는 표준 OSC inventory와 분리해 명시한다.

- OSC 7은 현재 표면이 소유하는 directory event를 URI와 그 로컬 `path`(터미널 런타임 (soksak core `docs/spec/terminal-runtime.ko.md`))와 함께 낸다. 빈 URI, 다른 스킴, 디코딩할 수 없는 경로는 거부한다.
- OSC 8은 `id=` parameter를 파싱해 typed hyperlink open/close event를 낸다. 지원하지 않는 parameter나 중복 parameter는 거부하며 빈 URI는 현재 link를 닫는다.
- OSC 9는 notification event를 낸다. 빈 notification은 거부하며 payload를 실행하지 않는다. 터미널 페이지는 각 알림을 `tab.notify`로 넘기며, 이는 보이지 않는 탭에 탭 알림과 시스템 알림을 둔다(플러그인 (soksak core `docs/spec/plugins.ko.md#탭-알림`)).
- OSC 133은 네 가지 typed shell marker(`prompt.start`, `prompt.end`, `command.start`, `command.finished`)와 문자열 parameter를 내고, 터미널 런타임 (soksak core `docs/spec/terminal-runtime.ko.md`)이 크기 변경에 쓰는 엔진 셸 상태를 정한다. 알 수 없는 marker와 `0`, `1`, `last`가 아닌 `redraw` 값은 거부한다.
- OSC 52는 계속 policy-gated clipboard 소유권을 사용한다. 조회 token은 한 번만 사용할 수 있고 거부하면 소유권을 지우며 BEL/ST 분할 입력을 검사한다.
- OSC 1337은 제한된 image 전송으로 유지한다. 잘못된 payload는 명시적으로 거부하고 multipart 상태는 표면을 넘지 않으며, 해당 표면이 소유하지 않은 이름의 삭제는 거부한다. 그림은 같은 출력 조각의 뒤 출력이 커서를 옮겨도 그 시퀀스가 나타난 자리의 커서와 스크롤 위치에 놓이며, 페이지의 `image.inline`과 `image.inline.deleted` 이벤트는 그 변경을 그린 래스터와 함께 보낸다. 그리기가 실패하면 그리기 오류 뒤에 보낸다.

여섯 계약 모두 BEL/ST 분할 입력을 보존한다. relay 응답은 원본 표면 식별자를 포함하며 vendor event를 다른 표면에 방송하지 않는다. 터미널 페이지는 typed directory·hyperlink·notification·shell-state event를 소비해 session status에 기록한다. 미래의 알 수 없는 event는 버리지 않고 `unsupported`에 남긴다.

## OSC·CSI 전체 요구사항

## CSI selector 근거

현재 사이드카는 실행 동작 근거가 있는 선택자만 기록한다: CSI `3C`, `?12h/l`, `?25h/l`, `CSI 1–7 SP q`(DECSCUSR), `m`, `?1049h/l`, `r`와 함께 사용하는 `S/T`, `J/K`, `@/P`, `L/M`, `I/Z`, `6n/c`, `b`, `?1/?1000/?1002/?1003/?1006/?2004 h/l`, `14t`, 그리고 VT 키패드 제어 `ESC =`/`ESC >`. `CSI 14t`는 글자 영역을 인라인 그림 픽셀 크기와 같은 단위인 기기 픽셀로 알린다. 그래서 이 값으로 셀 크기를 계산한 프로그램은 셀에 맞는 그림을 요청한다. 이는 CSI 전체 구현 주장이 아니다. 이동·지우기·모드·조회·마우스·보호 셀·속성 스택 중 목록에 없는 범주는 이름 있는 동작 검사나 명시적 거부 검사가 생길 때까지 미완료로 남긴다.

고정한 XTerm 기준 문서에 대해 OSC와 CSI를 서로 분리한 두 목록으로 관리한다.

- OSC: 기준 문서의 표준 선택자를 전부 목록화하고, 선택한 벤더 확장도 별도로 목록화한다(제목, 색상, 하이퍼링크, 클립보드, 알림, 셸 메타데이터, 그래픽 포함). 각 선택자는 효과 검사, 조회 가능한 경우 응답 검사, 정책 또는 플랫폼이 금지하는 경우 명시적 거부 검사를 가진다.
- CSI: 기준 문서의 표준 final byte, 매개변수 형식, private mode, 장치 응답 시퀀스를 전부 목록화한다. 커서 이동·표시, 지우기, 삽입·삭제, 스크롤, 스크롤 영역, 탭, 모드, 상태 보고, alternate screen 제어를 포함한다. 각 시퀀스는 효과·응답 또는 명시적 거부 검사를 가져야 하며 파서가 받아들이는 것만으로는 근거가 되지 않는다.

스크롤은 OSC가 아니라 CSI다. 최소 스크롤 계약에는 `CSI Ps S`(위로 스크롤), `CSI Ps T`(아래로 스크롤), `CSI Ps ; Ps r`(스크롤 영역 설정·초기화), `CSI Ps J`와 `CSI Ps K`(표시 그리드와 상호작용하는 지우기), 그리고 대응하는 커서·alternate screen 의미가 포함된다. 스크롤백 결과, 보이는 그리드, 커서 위치, 선택 동작은 서로 별도로 검사한다.

목록의 모든 항목에 이름 있는 테스트, 개별 제한 시간, 관측 가능한 성공·실패 결과, 호스트가 관여하는 Tauri·Wails 양쪽 구현 근거가 있을 때만 체크리스트를 완료로 표시한다. 파서가 받아들인 뒤 무시하는 시퀀스는 지원으로 취급하지 않는다.

## 입력과 권한

명시적 사용자 붙여넣기는 클립보드를 읽을 수 있다. 터미널 프로그램의 OSC 클립보드 요청은 사용자 동의가 아니며 설정된 정책과 금지 시 관측 가능한 거부를 요구한다. 파일·이미지 붙여넣기는 인용한 경로를 실행 없이 삽입한다. bracketed paste는 터미널 모드를 따르며 원문을 정확히 한 번 보낸다. 잘못된 인코딩은 앞부분만 받아들이지 않고 요청 전체를 거부한다.

조합 문자의 선택 범위는 UTF-16 위치와 길이를 사용한다. 대체 범위는 새 조합 문자열이 아닌 입력 클라이언트의 기존 텍스트를 가리킨다. 그래핌 분리와 셀 폭은 Unicode 지원 라이브러리를 사용하며 수작업 코드포인트 범위를 폭 계약으로 삼지 않는다.
