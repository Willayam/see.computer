#!/bin/sh
# Score a manifest with the vocabulary off and at each boost margin. Terms are
# always scored against the real vocabulary, so the off row is comparable.
set -eu

if [ "$#" -lt 2 ]; then
    echo "usage: $0 <binary> <manifest> [margins...]" >&2
    exit 1
fi
binary=$1
manifest=$2
shift 2
if [ "$#" -eq 0 ]; then
    set -- 1 2 3
fi
case $binary in
    /*) ;;
    *) binary=$(cd "$(dirname "$binary")" && pwd)/$(basename "$binary") ;;
esac
case $manifest in
    /*) ;;
    *) manifest=$(cd "$(dirname "$manifest")" && pwd)/$(basename "$manifest") ;;
esac
output_dir=$(mktemp -d)
terms=${SEE_COMPUTER_VOCABULARY:-$HOME/Documents/see.computer/vocabulary.md}
trap 'printf "per-clip output: %s\n" "$output_dir"' 0
SEE_COMPUTER_VOCABULARY="$output_dir/missing-vocabulary.md" "$binary" eval "$manifest" "$terms" > "$output_dir/disabled.tsv"
printf 'disabled\t%s\n' "$(tail -n 1 "$output_dir/disabled.tsv")"
for margin do
    SEE_COMPUTER_BOOST_MARGIN="$margin" "$binary" eval "$manifest" "$terms" > "$output_dir/margin-$margin.tsv"
    printf '%s\t%s\n' "$margin" "$(tail -n 1 "$output_dir/margin-$margin.tsv")"
done
