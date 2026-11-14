import type { libEn } from "./lib.en";

export const libKo: Partial<Record<keyof typeof libEn, string>> = {
  "lib.variant.domestic": "국내판",
  "lib.variant.international": "국제판",
  "lib.variant.appInternational": "WorkBuddy 국제판",
  "lib.variant.codebuddyIdeInternational": "CodeBuddy IDE 국제판",
  "lib.tools.workbuddy.label": "WorkBuddy",
  "lib.tools.workbuddy.description": "WorkBuddy 데스크톱 클라이언트의 계정 전환과 세션 복사",
  "lib.tools.codebuddyIde.label": "CodeBuddy IDE",
  "lib.tools.codebuddyIde.description": "CodeBuddy IDE 데스크톱 클라이언트(국내판 / 국제판)의 계정 전환",
  "lib.tools.codebuddyCli.label": "CodeBuddy CLI",
  "lib.tools.codebuddyCli.description": "CodeBuddy CLI의 기본 계정과 자동 로테이션",
  "lib.tools.vscodeExt.label": "VS Code CodeBuddy 플러그인",
  "lib.tools.vscodeExt.description": "VS Code 안 CodeBuddy 플러그인의 계정 전환과 세션 복사",
  "lib.tools.jetbrains.label": "JetBrains IDE 플러그인(IDEA / PyCharm)",
  "lib.tools.jetbrains.description": "IntelliJ IDEA와 PyCharm 안 CodeBuddy 플러그인의 계정 전환",
};
