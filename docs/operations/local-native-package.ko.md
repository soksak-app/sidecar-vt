# Local native package 개발

Sidecar manifest는 배포 dependency를 선언된 core Git revision에 고정한다.
개발 중에는 이 pin을 바꾸거나 pnpm이 생성한 link override를 commit하지 않고
local core checkout의 native package를 연결할 수 있다.

Local package를 연결하기 전에 dependency resolution에 영향을 받는 저장소
검사를 실행한다. `pnpm exec soksak-check records`는 pnpm resolution을 실행하며,
생성된 local link metadata를 제거한 뒤 manifest의 배포 pin을 다시 가져올 수
있다. 그 다음 아래 명령을 실행한다.

```sh
pnpm install --frozen-lockfile --no-optional
pnpm link <absolute-core-native-process-identity-package-directory>
node -e "const p=require.resolve('@soksak/native-process-identity-darwin/package.json');console.log(p,require(p).version)"
make build
make release OUT=<new-output-directory> SOK=<core>/target/debug/sok
```

`require.resolve`가 의도한 core checkout을 가리키는지 확인하고 package에
`include/soksak_process_identity.h`, `lib/libsoksak-process-identity.a`,
`lib/pkgconfig/soksak-process-identity.pc`가 있는지 확인한다. Local link가
적용된 뒤 `make build`와 `make release`를 실행한다. pnpm이 만든 workspace와
lock override는 version control에서 제외하고 `package.json`과 commit된
lockfile은 선언된 배포 pin을 유지한다. Dependency를 다시 resolve할 수 있는
pnpm 명령을 실행하기 전에 생성된 release를 보존한다.

Local link는 개발용 resolution일 뿐이다. 배포 pin의 fresh install 검증이나
sidecar 공개를 의미하지 않는다.
