# 개발 가이드

[English](./DEVELOPMENT.md) | **한국어**

## 요구 사항

Node.js 20 이상, 안정 버전 Rust 툴체인, macOS(또는 Windows / Linux).

## 명령어

```bash
npm install
npm run tauri dev          # 개발 모드
npm run build:app          # 프런트엔드 리소스 패치를 적용한 debug .app 빌드
npm run build:app:release  # release .app 및 서명된 업데이트 패키지 빌드
```

## 새 버전 릴리스

자동 업데이트용 서명 키는 `~/.wb-switch/wb-switch-updater.key`에 있습니다. 빌드 스크립트는 `TAURI_SIGNING_PRIVATE_KEY`로 이 키를 읽습니다.

1. `npm run build:app:release`를 실행합니다. `.app.tar.gz`와 `.sig`가 생성됩니다. Windows NSIS 빌드는 현재 버전의 `*_x64-setup.exe`와 `.exe.sig`도 함께 생성합니다. CI는 먼저 `target/**/release/bundle`을 삭제하므로, cargo 캐시에 남은 이전 설치 파일이 릴리스에 섞이지 않습니다.
2. macOS: `UPDATE_OS=macos UPDATE_ARCH=aarch64 sh scripts/gen-update-json.sh`를 실행해 `latest-macos-aarch64.json`을 생성합니다. Intel은 `UPDATE_ARCH=x86_64`를 사용합니다.
3. Windows: `UPDATE_OS=windows UPDATE_ARCH=x86_64 sh scripts/gen-update-json.sh`를 실행해 `latest-windows-x86_64.json`을 생성합니다.
4. `python3 scripts/merge-update-manifests.py <출력 디렉터리>`를 실행해 `latest.json`으로 병합합니다. 이 스크립트는 Windows 항목을 `latest-macos-x86_64.json`에도 복사하므로, 이미 설치된 Windows 클라이언트가 계속 업데이트됩니다.
5. 설치 파일, 서명된 업데이트 패키지, `latest*.json`을 GitHub Release에 업로드합니다.

### npm 버전(webui) 릴리스

1. 서버 바이너리를 컴파일해 GitHub Release에 업로드합니다. `.github/workflows/build.yml`이 이 작업을 자동으로 수행합니다.
2. `cd npm && npm publish`를 실행합니다. 패키지 이름은 `workbuddy-switch`이며, postinstall 단계에서 현재 플랫폼용 바이너리를 릴리스에서 내려받습니다.

## 디렉터리 구조

```
src-tauri/
  src/
    commands.rs      # 기존 Python HTTP API를 대응하는 얇은 Tauri command 래퍼
crates/
  wb-switch-core/    # 핵심 로직, 관심사별 모듈 (account, auth_file, oauth,
                     # process, switch, session, checkin, refresh, update, config 등)
  wb-switch-server/  # HTTP 서버와 CLI: axum API + rust-embed 프런트엔드
src/                 # 프런트엔드: components, pages, lib, stores
                     # lib/api.ts는 Tauri invoke와 HTTP fetch 두 채널을 사용
npm/                 # npm 패키지: package.json, bin, scripts/install.js
```

## 개인정보 주의 사항

- 로컬 데이터를 커밋하지 않습니다. `.gitignore`가 `accounts.json`, 인증 파일, 키, 토큰을 제외합니다.
- 릴리스 전에 `git grep`으로 토큰 패턴을 검사합니다. `ghp_`, `npm_`, `gho_` 등의 접두사를 찾습니다.
