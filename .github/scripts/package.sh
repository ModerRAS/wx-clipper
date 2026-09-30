#!/usr/bin/env bash
set -euo pipefail

target="${TARGET:?}"
archive="${ARCHIVE:?}"
bin="target/${target}/release/wx-clipper"
mkdir -p dist

if [[ "$archive" == "zip" ]]; then
  out="dist/wx-clipper-${target}.zip"
  if command -v python3 >/dev/null 2>&1; then
    py=python3
  else
    py=python
  fi
  BIN="${bin}.exe" OUT="$out" "$py" -c 'import os, zipfile
src = os.environ["BIN"]
out = os.environ["OUT"]
with zipfile.ZipFile(out, "w", compression=zipfile.ZIP_DEFLATED) as bundle:
    bundle.write(src, "wx-clipper.exe")
with zipfile.ZipFile(out) as bundle:
    names = bundle.namelist()
if names != ["wx-clipper.exe"]:
    raise SystemExit("zip 内容应为 wx-clipper.exe，实际是 %s" % names)
'
else
  out="dist/wx-clipper-${target}.tar.gz"
  stage=$(mktemp -d)
  cp "$bin" "$stage/wx-clipper"
  chmod +x "$stage/wx-clipper"
  COPYFILE_DISABLE=1 tar -C "$stage" -czf "$out" wx-clipper
  names=$(tar -tzf "$out")
  if [[ "$names" != "wx-clipper" ]]; then
    echo "tar.gz 内容应为 wx-clipper，实际是 ${names}"
    exit 1
  fi
fi
