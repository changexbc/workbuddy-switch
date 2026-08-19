:; exec node "$(dirname "$0")/helper.cjs" "$@"
@echo off
rem Windows CodeBuddy CLI apiKeyHelper for wb-switch.
rem 核心逻辑在 helper.cjs；本文件只是启动跳板。
rem 首行是 bash/cmd 双兼容的 polyglot：Git Bash 下由 bash 执行并 exec node，
rem PowerShell/cmd 下被当作标签跳过，走下面这行。
node "%~dp0helper.cjs" %*
