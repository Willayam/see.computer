#!/bin/sh
# Arguments: prepared replay directory, AOSP proof directory, iikb efa3c734 checkout.
# The Swedish iikb manifest asset belongs at <replay>/sv.lexicon.
set -eu
out=$(cd "$1" && pwd)
aosp=$(cd "$2" && pwd)
iikb=$(cd "$3" && pwd)
here=$(cd "$(dirname "$0")" && pwd)
clang++ -O2 -std=c++17 -DHOST_TOOL -fobjc-arc -framework Foundation \
    -I"$aosp/proof/src" "$here/aosp.mm" "$aosp/proof/macosx/liblatinime.a" -o "$out/aosp-run"
python3 "$here/dictionary.py" "$here/../../Keyboard/words-en.bin" > "$out/ours.combined"
java -Xmx3g -jar "$aosp/community/dicttool_aosp.jar" makedict -s "$out/ours.combined" -d "$out/ours.dict"
python3 "$here/spaces.py" "$out"
english="$aosp/aosp/java/res/raw/main_en.dict"
"$out/aosp-run" "$out/english.json" "$out/en.json" "$english" "$out/aosp-en.json"
"$out/aosp-run" "$out/english.json" "$out/continuous.json" "$english" "$out/aosp-free.json"
"$out/aosp-run" "$out/english.json" "$out/en.json" "$out/ours.dict" "$out/matched-en.json"
"$out/aosp-run" "$out/bilingual.json" "$out/sv.json" "$aosp/community/main_sv.dict" "$out/aosp-sv.json"
package=$(mktemp -d)
trap 'rm -rf "$package"' EXIT
mkdir -p "$package/Sources/Race"
for target in KeyboardCore KeyboardEnglish; do
    ln -s "$iikb/Packages/KeyboardModules/Sources/$target" "$package/Sources/$target"
done
cp "$here/iikb.swift" "$package/Sources/Race/"
cat > "$package/Package.swift" <<'SWIFT'
// swift-tools-version: 6.2
import PackageDescription
let package = Package(name: "Race", platforms: [.macOS(.v14)], targets: [
    .target(name: "KeyboardCore"),
    .target(name: "KeyboardEnglish", dependencies: ["KeyboardCore"], resources: [.process("Resources")]),
    .executableTarget(name: "Race", dependencies: ["KeyboardCore","KeyboardEnglish"])
])
SWIFT
cd "$package"
swift build -c release
runner="$(swift build -c release --show-bin-path)/Race"
"$runner" "$out/english.json" "$out/en.json" "$out/iikb-en.json"
"$runner" "$out/english.json" "$out/en.json" "$out/reset-en.json" --reset-touches
"$runner" "$out/bilingual.json" "$out/sv.json" "$out/iikb-sv.json" "$out/sv.lexicon"
"$runner" "$out/english.json" "$out/tsi.json" "$out/iikb-free.json" --observed-spaces
"$runner" "$out/english.json" "$out/tsi.json" "$out/iikb-free-reset.json" --observed-spaces --reset-touches
# Build the boundary-repair ablation from a private copy of the same donor sources.
rm "$package/Sources/KeyboardCore"
cp -R "$iikb/Packages/KeyboardModules/Sources/KeyboardCore" "$package/Sources/KeyboardCore"
python3 - <<'PY'
from pathlib import Path
p = Path("Sources/KeyboardCore/Correction/Corrector.swift")
code = p.read_text()
for method in [
    "func missedSpaces(_ typed: TypedWord, context: String? = nil) -> (words: String, odds: Double)? {",
    "func spaced(_ run: TypedWord, context: String, correcting: Bool) -> String? {",
]:
    assert code.count(method) == 1
    code = code.replace(method, method + "\n        return nil")
p.write_text(code)
PY
swift build -c release
"$runner" "$out/english.json" "$out/tsi.json" "$out/verified-no-space.json" --observed-spaces
