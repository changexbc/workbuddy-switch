# Agent Companion 데스크톱 컴포넌트

[English](./agent-companion-integration.md) | **한국어**

데스크톱 빌드는 `vendor/agent-companion/`에 고정된 Agent Companion 소스 스냅샷 하나를 포함합니다. 리비전과 아카이브 다이제스트는 `vendor/agent-companion.version.json`에 기록됩니다. Rust 플러그인, 임베드된 페이지, 모니터 런타임은 모두 이 스냅샷에서 나옵니다. 일반 WebUI의 `npm run build`는 네이티브 컴포넌트를 준비하거나 시작하지 않습니다.

## 고정된 소스 갱신

Agent Companion 변경 사항을 커밋한 뒤 다음을 실행합니다.

```sh
node scripts/vendor-agent-companion.mjs /path/to/agent-companion <full-commit-sha>
```

이 스크립트는 `git archive`를 사용하므로 로컬의 커밋되지 않은 변경은 스냅샷에 들어가지 않습니다. 생성된 `vendor/` 파일을 매니페스트, 호스트 변경과 함께 커밋하세요. 그러면 깨끗한 wb-switch 체크아웃에는 별도의 저장소가 필요하지 않습니다. 매니페스트의 `interfaceVersion`은 호스트와 컴포넌트의 호환 경계입니다. 네이티브 플러그인 커맨드나 창 동작을 바꾸려면 호스트 릴리스가 필요합니다.

## 빌드

`tauri dev`는 `npm run dev:desktop`을 실행합니다. `tauri build`는 `npm run build:desktop`을 실행합니다. 두 명령 모두 고정된 프런트엔드 의존성을 해당 lockfile로 설치하고, 런타임을 `--locked`로 컴파일합니다. 빌드는 `dist-embed/` 출력 전체를 `companion/` 아래로 복사하며, 두 HTML 진입점과 그 상대 리소스를 모두 보존합니다. Tauri는 대상 플랫폼에 맞는 런타임을 외부 바이너리로 번들합니다. 크로스 타깃 빌드에서는 `AGENT_COMPANION_TARGET`을 `tauri build --target`에 넘긴 것과 같은 Rust 트리플로 설정하세요. 릴리스 워크플로는 플랫폼마다 이 값을 설정합니다.

설치된 앱에는 wb-switch 트레이가 하나만 있습니다. 내장 컴포넌트는 별도의 트레이나 로그인 항목을 만들지 않습니다. 패키징된 파일은 나중에 UI와 런타임을 따로 업데이트할 때의 기준본이며, 현재 통합은 런타임에 실행 코드를 내려받지 않습니다.

## 공개 데모 오버레이

읽기 전용 데모 페이지(`npm run build:demo`)는 컴포넌트의 *웹 데모* 빌드도 함께 임베드합니다. 따라서 방문자는 아무것도 설치하지 않고 레일을 볼 수 있습니다. `scripts/prepare-companion-demo.mjs`는 고정된 리비전이 `vendor/agent-companion/dist-demo/` 안에 기록된 마커와 다를 때만 해당 디렉터리를 다시 빌드합니다. 각 모드는 두 출력을 분리해 유지합니다. `dev`는 결과물을 `public/companion-demo/`로 복사하고(gitignore 대상, Vite 개발 서버용), `build`는 아무것도 복사하지 않고 그 개발용 복사본을 지우며, `copy`는 Vite 빌드 후 결과물을 `dist/`에 넣고, `clean`은 둘 다 제거합니다. `npm run dev:demo`는 `dev`를 먼저 실행하고, `npm run build:demo`는 `build` 다음 `copy`를 실행합니다. 프로덕션 빌드(`npm run build`, 따라서 `build:desktop`도 포함)는 `clean`을 먼저 실행하므로, 오래된 개발용 복사본이 `public/`을 통해 프로덕션 번들에 섞여 들어갈 수 없습니다.

`src/components/companion-demo-dialog.tsx`는 `companion-demo/desktop.html`을 iframe에 렌더링하고, `src/lib/companion-demo-script.ts`의 가상 스냅샷으로 업스트림 `agent-companion-demo` postMessage 채널을 통해 제어합니다. 이 프레임은 데모 빌드 상태의 업스트림 레일입니다. 호스트도, 트레이도, 로컬 데이터도 없으며, 세션 링크나 닫기 명령은 `blocked`로만 응답합니다. 첫 스냅샷은 프레임의 `ready` 메시지에서 보내세요. iframe의 `load` 이벤트는 너무 이릅니다. 데모 브리지는 동적 임포트이고 `load` 이후에 리스너를 등록하므로, 그때 보낸 스냅샷은 버려집니다.

오버레이와 사이드바 항목(`CompanionDemoFooter`)은 데모 빌드에서만 렌더링됩니다. 데스크톱과 WebUI는 기존 동작을 유지합니다.
