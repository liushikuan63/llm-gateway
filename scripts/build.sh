#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*)
    exec powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/build-win.ps1
    ;;
  *)
    echo "This release pipeline only produces Windows NSIS/MSI installers. Run scripts/build-win.ps1 on Windows." >&2
    exit 1
    ;;
esac
