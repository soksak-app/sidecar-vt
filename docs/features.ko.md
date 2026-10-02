# 기능

[English](features.md)

- [o] S1 — P1: 이 sidecar를 자기 repository로 build, test, release한다. 2026-10-02에 soksak core repository(그곳의 checklist 항목 R1-5-2)에서 옮겼으며, 이전 변경 이력은 core에 있다. macOS arm64에서 `make test`, `make build`, `make release`가 통과한다.
- [o] S2 — P1: 같은 영속 연결에서 closed 알림을 받은 표면을 다시 연다. 2026-10-02 core window check `library windows create and open projects in place`에서 발견: 터미널을 보인 창을 닫은 프로젝트를 새 창에서 보일 수 없었다. 터미널 open이 `stale attachment`로 거부되어 호스트가 오지 않는 터미널 그림을 기다렸다. closed 알림은 registry 항목을 지웠지만 표면의 송신기와 부착 판을 연결에 남겨, 다음 open이 항목이 없는 registry와 옛 판을 비교했다. 2026-10-02 완료: 닫기가 성공하면 이 둘도 지운다. Red: `a_surface_reopened_after_its_closed_notice_is_not_a_stale_attachment`가 `stale attachment` 응답 두 개를 받았다. Green: 그 test와 `make test`가 macOS arm64에서 통과한다.
