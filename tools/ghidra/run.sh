#!/bin/sh
# Headless-decompile one client binary: tools/ghidra/run.sh <binary>
# Output: decompiled/native/<binary>.c
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
export JAVA_HOME=$(ls -d "$ROOT"/_tools/jdk-21*)
GHIDRA=/c/Users/quinn/Downloads/ghidra_12.1.3_PUBLIC
name=$(basename "$1")
echo | cmd //c "$(cygpath -w $GHIDRA/support/analyzeHeadless.bat)" "$(cygpath -w $ROOT/_tools/ghidra_proj)" "$name" \
  -import "$(cygpath -w "$1")" -overwrite \
  -scriptPath "$(cygpath -w $ROOT/tools/ghidra)" -postScript PyMethodFunctions.java -postScript DataRefFunctions.java -postScript DecompileAll.java "$(cygpath -w $ROOT/decompiled/native)"
