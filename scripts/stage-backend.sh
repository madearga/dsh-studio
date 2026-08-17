#!/bin/sh
# Stage a private node + dsh copy into resources/backend so the app is
# self-contained. Preference order for the dsh files:
#   1. DSH_SOURCE  — an explicit node_modules directory
#   2. any npx cache holding a complete @deepseek-ai/dsh
#   3. fresh npm install
set -e
cd "$(dirname "$0")/.."

stage=resources/backend
mkdir -p "$stage"
export npm_config_cache="$PWD/.npm-cache"

# Platform bits: ditto on macOS, cp -r elsewhere; .exe suffix on Windows.
case "$(uname -s)" in
  Darwin*) EXE="";       COPY="ditto" ;;
  MINGW*|MSYS*|CYGWIN*) EXE=".exe";  COPY="cp -R" ;;
  *) EXE="";             COPY="cp -R" ;;
esac

cp "$(node -p 'process.execPath')" "$stage/node$EXE"
if command -v codesign >/dev/null 2>&1; then
  codesign --force --sign - "$stage/node$EXE" 2>/dev/null || true
fi

src="${DSH_SOURCE:-}"
[ -z "$src" ] && for d in "$HOME"/.npm/_npx/*/node_modules; do
  [ -f "$d/@deepseek-ai/dsh/lib/bin.js" ] && src="$d" && break
done

if [ -n "$src" ]; then
  echo "staging dsh from $src"
  $COPY "$src" "$stage/node_modules"
else
  echo "installing @deepseek-ai/dsh from npm"
  (cd "$stage" && npm init -y >/dev/null && npm i @deepseek-ai/dsh)
fi

"$stage/node$EXE" "$stage/node_modules/@deepseek-ai/dsh/lib/bin.js" --version
du -sh "$stage"
