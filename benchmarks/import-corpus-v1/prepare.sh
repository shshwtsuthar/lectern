#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C
umask 022

readonly recipe_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
readonly repository_root=$(cd -- "$recipe_dir/../.." && pwd)
readonly source_manifest="$recipe_dir/sources.tsv"
readonly work_root="$repository_root/target/benchmarks/import-corpus-v1"
readonly seed_root="$work_root/seeds"
readonly corpus_root="$work_root/corpus"
readonly generation_plan="$work_root/generation-plan.tsv"
readonly corpus_manifest="$work_root/corpus-manifest.json"

readonly epub_total=7000
readonly pdf_total=3000
readonly expected_seed_files=48
readonly expected_seed_bytes=78249219
readonly expected_corpus_bytes=9742451988
readonly target_limit_bytes=$((15 * 1024 * 1024 * 1024))
readonly hard_limit_bytes=$((20 * 1024 * 1024 * 1024))
readonly free_space_reserve_bytes=$((40 * 1024 * 1024 * 1024))
readonly maximum_seed_bytes=$((100 * 1024 * 1024))

jobs=${LECTERN_CORPUS_JOBS:-4}
mode=prepare

usage() {
    cat <<'EOF'
Prepare Lectern's byte-pinned 10,000-file import benchmark corpus.

Usage:
  benchmarks/import-corpus-v1/prepare.sh [--check-manifest]

Environment:
  LECTERN_CORPUS_JOBS  Concurrent downloads and copies (default: 4)
EOF
}

if (($# > 1)); then
    usage >&2
    exit 2
fi
if (($# == 1)); then
    case $1 in
        --check-manifest) mode=check ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            printf 'unknown option: %s\n' "$1" >&2
            usage >&2
            exit 2
            ;;
    esac
fi
if [[ ! $jobs =~ ^[1-9][0-9]*$ ]]; then
    printf 'LECTERN_CORPUS_JOBS must be a positive integer, got %s\n' "$jobs" >&2
    exit 2
fi

declare -A seed_size=()
declare -A seed_sha=()
declare -A expected_seed_path=()
declare -a epub_small=() epub_medium=() epub_large=()
declare -a pdf_small=() pdf_medium=() pdf_large=()
manifest_rows=0
manifest_epub=0
manifest_pdf=0
manifest_seed_bytes=0
review_rows=0

sort_array() {
    local array_name=$1
    declare -n array_values=$array_name
    local -a sorted_values=()
    mapfile -t sorted_values < <(printf '%s\n' "${array_values[@]}" | sort)
    array_values=("${sorted_values[@]}")
}

validate_manifest() {
    local expected_header
    expected_header=$'format\tfilename\tbytes\tsha256\turl\tsource_revision\trights_basis\trights_url\tdeclared_rights\treview_required'
    if [[ ! -f $source_manifest ]]; then
        printf 'source manifest is missing: %s\n' "$source_manifest" >&2
        exit 1
    fi
    if [[ $(head -n 1 "$source_manifest") != "$expected_header" ]]; then
        printf 'unexpected source manifest header\n' >&2
        exit 1
    fi

    local format filename bytes sha256 url source_revision rights_basis rights_url
    local declared_rights review_required extra key
    while IFS=$'\t' read -r format filename bytes sha256 url source_revision rights_basis \
        rights_url declared_rights review_required extra; do
        if [[ -n ${extra:-} ]]; then
            printf 'too many fields in source row for %s\n' "$filename" >&2
            exit 1
        fi
        case $format in
            epub)
                [[ $filename == *.epub ]] || {
                    printf 'EPUB seed has unexpected filename: %s\n' "$filename" >&2
                    exit 1
                }
                ((manifest_epub += 1))
                ;;
            pdf)
                [[ $filename == *.pdf ]] || {
                    printf 'PDF seed has unexpected filename: %s\n' "$filename" >&2
                    exit 1
                }
                ((manifest_pdf += 1))
                ;;
            *)
                printf 'unsupported source format: %s\n' "$format" >&2
                exit 1
                ;;
        esac
        if [[ -z $filename || $filename == */* || $filename == .* ]]; then
            printf 'unsafe source filename: %s\n' "$filename" >&2
            exit 1
        fi
        if [[ ! $bytes =~ ^[1-9][0-9]*$ ]] || ((bytes > maximum_seed_bytes)); then
            printf 'invalid source byte length for %s: %s\n' "$filename" "$bytes" >&2
            exit 1
        fi
        if [[ ! $sha256 =~ ^[0-9a-f]{64}$ ]]; then
            printf 'invalid SHA-256 for %s\n' "$filename" >&2
            exit 1
        fi
        if [[ $url != https://* || $rights_url != https://* ]]; then
            printf 'source and rights URLs must use HTTPS for %s\n' "$filename" >&2
            exit 1
        fi
        if [[ -z $source_revision || -z $rights_basis || -z $declared_rights ]]; then
            printf 'incomplete provenance for %s\n' "$filename" >&2
            exit 1
        fi
        if [[ $review_required != yes && $review_required != no ]]; then
            printf 'invalid review_required value for %s\n' "$filename" >&2
            exit 1
        fi

        key="$format/$filename"
        if [[ -v seed_size["$key"] ]]; then
            printf 'duplicate source manifest entry: %s\n' "$key" >&2
            exit 1
        fi
        seed_size["$key"]=$bytes
        seed_sha["$key"]=$sha256
        expected_seed_path["seeds/$key"]=1
        ((manifest_rows += 1))
        ((manifest_seed_bytes += bytes))
        [[ $review_required == yes ]] && ((review_rows += 1))

        if [[ $format == epub ]]; then
            if ((bytes <= 750 * 1024)); then
                epub_small+=("$key")
            elif ((bytes <= 2560 * 1024)); then
                epub_medium+=("$key")
            elif ((bytes <= 6 * 1024 * 1024)); then
                epub_large+=("$key")
            else
                printf 'EPUB seed is outside supported size buckets: %s\n' "$filename" >&2
                exit 1
            fi
        elif ((bytes <= 1024 * 1024)); then
            pdf_small+=("$key")
        elif ((bytes <= 2 * 1024 * 1024)); then
            pdf_medium+=("$key")
        elif ((bytes <= 10 * 1024 * 1024)); then
            pdf_large+=("$key")
        else
            printf 'PDF seed is outside supported size buckets: %s\n' "$filename" >&2
            exit 1
        fi
    done < <(tail -n +2 "$source_manifest")

    if ((manifest_rows != expected_seed_files || manifest_epub != 36 || manifest_pdf != 12)); then
        printf 'source count mismatch: rows=%d EPUB=%d PDF=%d\n' \
            "$manifest_rows" "$manifest_epub" "$manifest_pdf" >&2
        exit 1
    fi
    if ((manifest_seed_bytes != expected_seed_bytes)); then
        printf 'source byte total mismatch: expected %d, got %d\n' \
            "$expected_seed_bytes" "$manifest_seed_bytes" >&2
        exit 1
    fi
    if ((review_rows != 3)); then
        printf 'expected three review-required sources, got %d\n' "$review_rows" >&2
        exit 1
    fi
    for bucket in epub_small epub_medium epub_large pdf_small pdf_medium pdf_large; do
        declare -n bucket_values=$bucket
        if ((${#bucket_values[@]} == 0)); then
            printf 'source size bucket is empty: %s\n' "$bucket" >&2
            exit 1
        fi
        sort_array "$bucket"
    done
}

bucket_projected_bytes() {
    local bucket_name=$1
    local copies=$2
    declare -n bucket_values=$bucket_name
    local bucket_sum=0
    local key
    for key in "${bucket_values[@]}"; do
        ((bucket_sum += seed_size["$key"]))
    done
    local full_cycles=$((copies / ${#bucket_values[@]}))
    local remainder=$((copies % ${#bucket_values[@]}))
    local total=$((full_cycles * bucket_sum))
    local index
    for ((index = 0; index < remainder; index++)); do
        ((total += seed_size["${bucket_values[index]}"]))
    done
    printf '%d\n' "$total"
}

calculate_projected_bytes() {
    local total=0 value
    for specification in \
        'epub_small 4900' 'epub_medium 1750' 'epub_large 350' \
        'pdf_small 2100' 'pdf_medium 750' 'pdf_large 150'; do
        read -r bucket copies <<<"$specification"
        value=$(bucket_projected_bytes "$bucket" "$copies")
        ((total += value))
    done
    printf '%d\n' "$total"
}

validate_manifest
projected_bytes=$(calculate_projected_bytes)
if ((projected_bytes != expected_corpus_bytes)); then
    printf 'projected corpus mismatch: expected %d, got %d\n' \
        "$expected_corpus_bytes" "$projected_bytes" >&2
    exit 1
fi

if [[ $mode == check ]]; then
    printf 'manifest_ok rows=%d epub=%d pdf=%d seed_bytes=%d projected_bytes=%d review_required=%d\n' \
        "$manifest_rows" "$manifest_epub" "$manifest_pdf" "$manifest_seed_bytes" \
        "$projected_bytes" "$review_rows"
    exit 0
fi

require_command() {
    command -v "$1" >/dev/null 2>&1 || {
        printf 'required command is unavailable: %s\n' "$1" >&2
        exit 1
    }
}

for command_name in awk cmp cp curl df du find mkdir mktemp mv qpdf sha256sum sort stat \
    tail unzip wc xargs; do
    require_command "$command_name"
done

mkdir -p "$seed_root/epub" "$seed_root/pdf" "$work_root"

verify_seed_bytes() {
    local path=$1
    local expected_bytes=$2
    local expected_sha=$3
    local actual_bytes actual_sha ignored
    actual_bytes=$(stat -c '%s' "$path")
    read -r actual_sha ignored < <(sha256sum "$path")
    [[ $actual_bytes == "$expected_bytes" && $actual_sha == "$expected_sha" ]]
}

validate_seed_container() {
    local format=$1
    local path=$2
    if [[ $format == epub ]]; then
        unzip -tqq "$path"
    else
        local qpdf_status=0
        qpdf --check "$path" >/dev/null 2>&1 || qpdf_status=$?
        if ((qpdf_status != 0 && qpdf_status != 3)); then
            printf 'qpdf rejected seed with status %d: %s\n' "$qpdf_status" "$path" >&2
            return "$qpdf_status"
        fi
        if ((qpdf_status == 3)); then
            printf 'qpdf_warning\t%s\n' "${path##*/}" >&2
        fi
    fi
}

download_seed() {
    set -euo pipefail
    local format=$1
    local filename=$2
    local expected_bytes=$3
    local expected_sha=$4
    local url=$5
    local destination="$seed_root/$format/$filename"
    local temporary="$destination.partial"

    if [[ -L $destination ]]; then
        printf 'refusing symlink seed: %s\n' "$destination" >&2
        return 1
    fi
    if [[ -e $destination ]]; then
        if verify_seed_bytes "$destination" "$expected_bytes" "$expected_sha"; then
            validate_seed_container "$format" "$destination"
            printf 'reuse\t%s\n' "$filename"
            return
        fi
        printf 'refusing to replace non-matching seed: %s\n' "$destination" >&2
        return 1
    fi
    if [[ -L $temporary ]]; then
        printf 'refusing symlink temporary file: %s\n' "$temporary" >&2
        return 1
    fi
    rm -f -- "$temporary"
    curl --fail --silent --show-error --location --retry 2 --retry-all-errors \
        --connect-timeout 15 --max-time 240 --max-filesize "$maximum_seed_bytes" \
        --user-agent 'Lectern benchmark corpus preparation; local testing' \
        --output "$temporary" "$url"
    if ! verify_seed_bytes "$temporary" "$expected_bytes" "$expected_sha"; then
        printf 'download integrity mismatch for %s\n' "$filename" >&2
        return 1
    fi
    validate_seed_container "$format" "$temporary"
    mv -- "$temporary" "$destination"
    printf 'downloaded\t%s\n' "$filename"
}
export seed_root maximum_seed_bytes
export -f verify_seed_bytes validate_seed_container download_seed

awk -F '\t' 'NR > 1 { for (field = 1; field <= 5; field++) printf "%s%c", $field, 0 }' \
    "$source_manifest" \
    | xargs -0 -r -n 5 -P "$jobs" bash -c 'download_seed "$1" "$2" "$3" "$4" "$5"' _

unexpected_link=$(find "$seed_root" -type l -print -quit)
if [[ -n $unexpected_link ]]; then
    printf 'seed tree contains a symlink: %s\n' "$unexpected_link" >&2
    exit 1
fi
actual_seed_files=0
while IFS= read -r -d '' path; do
    relative=${path#"$work_root/"}
    if [[ ! -v expected_seed_path["$relative"] ]]; then
        printf 'unexpected file in seed tree: %s\n' "$path" >&2
        exit 1
    fi
    ((actual_seed_files += 1))
done < <(find "$seed_root" -type f -print0)
if ((actual_seed_files != expected_seed_files)); then
    printf 'seed tree count mismatch: expected %d, got %d\n' \
        "$expected_seed_files" "$actual_seed_files" >&2
    exit 1
fi

plan_temporary="$generation_plan.partial"
if [[ -L $plan_temporary ]]; then
    printf 'refusing symlink plan file: %s\n' "$plan_temporary" >&2
    exit 1
fi
printf 'format\tindex\tseed_relative\tdestination_relative\tsize_bytes\tsha256\n' \
    >"$plan_temporary"
planned_count=0
planned_bytes=0

append_bucket() {
    local format=$1
    local start=$2
    local copies=$3
    local bucket_name=$4
    declare -n bucket_values=$bucket_name
    local offset index key filename base shard destination
    for ((offset = 0; offset < copies; offset++)); do
        index=$((start + offset))
        key=${bucket_values[offset % ${#bucket_values[@]}]}
        filename=${key#*/}
        base=${filename%.*}
        shard=$(printf '%02d' "$((index / 100))")
        destination=$(printf 'corpus/%s/%s/%s-%06d-%s.%s' \
            "$format" "$shard" "$format" "$index" "$base" "$format")
        printf '%s\t%d\tseeds/%s\t%s\t%d\t%s\n' \
            "$format" "$index" "$key" "$destination" \
            "${seed_size["$key"]}" "${seed_sha["$key"]}" >>"$plan_temporary"
        ((planned_count += 1))
        ((planned_bytes += seed_size["$key"]))
    done
}

append_bucket epub 0 4900 epub_small
append_bucket epub 4900 1750 epub_medium
append_bucket epub 6650 350 epub_large
append_bucket pdf 0 2100 pdf_small
append_bucket pdf 2100 750 pdf_medium
append_bucket pdf 2850 150 pdf_large

if ((planned_count != epub_total + pdf_total || planned_bytes != projected_bytes)); then
    printf 'generation plan mismatch: files=%d bytes=%d\n' "$planned_count" "$planned_bytes" >&2
    exit 1
fi
if ((projected_bytes > hard_limit_bytes)); then
    printf 'hard abort: projected corpus exceeds 20 GiB (%d bytes)\n' "$projected_bytes" >&2
    exit 1
fi
if ((projected_bytes > target_limit_bytes)); then
    printf 'projected corpus exceeds the 15 GiB target (%d bytes)\n' "$projected_bytes" >&2
    exit 1
fi
available_bytes=$(df --output=avail -B1 "$work_root" | tail -n 1 | tr -d ' ')
required_bytes=$((projected_bytes + free_space_reserve_bytes))
if ((available_bytes < required_bytes)); then
    printf 'insufficient free space: need %d bytes to preserve a 40 GiB reserve, have %d\n' \
        "$required_bytes" "$available_bytes" >&2
    exit 1
fi
mv -- "$plan_temporary" "$generation_plan"

for format in epub pdf; do
    count=$epub_total
    [[ $format == pdf ]] && count=$pdf_total
    last_shard=$(((count - 1) / 100))
    for ((shard = 0; shard <= last_shard; shard++)); do
        mkdir -p "$corpus_root/$format/$(printf '%02d' "$shard")"
    done
done

copy_seed() {
    set -euo pipefail
    local source="$work_root/$1"
    local destination="$work_root/$2"
    local temporary="$destination.partial"
    if [[ -e $destination || -L $destination ]]; then
        if [[ ! -L $destination && -f $destination ]] && cmp -s "$source" "$destination"; then
            return
        fi
        printf 'refusing to replace non-matching corpus file: %s\n' "$destination" >&2
        return 1
    fi
    if [[ -e $temporary || -L $temporary ]]; then
        printf 'refusing existing temporary corpus file: %s\n' "$temporary" >&2
        return 1
    fi
    cp --reflink=never -- "$source" "$temporary"
    mv -- "$temporary" "$destination"
}
export work_root
export -f copy_seed

awk -F '\t' 'NR > 1 { printf "%s%c%s%c", $3, 0, $4, 0 }' "$generation_plan" \
    | xargs -0 -r -n 2 -P "$jobs" bash -c 'copy_seed "$1" "$2"' _

unexpected_link=$(find "$corpus_root" -type l -print -quit)
if [[ -n $unexpected_link ]]; then
    printf 'corpus contains a symlink: %s\n' "$unexpected_link" >&2
    exit 1
fi
actual_epub=$(find "$corpus_root/epub" -type f -name '*.epub' | wc -l)
actual_pdf=$(find "$corpus_root/pdf" -type f -name '*.pdf' | wc -l)
actual_files=$(find "$corpus_root" -type f | wc -l)
actual_bytes=$(find "$corpus_root" -type f -printf '%s\n' \
    | awk '{ total += $1 } END { printf "%.0f", total }')
unique_inodes=$(find "$corpus_root" -type f -printf '%D:%i\n' | sort -u | wc -l)
if ((actual_epub != epub_total || actual_pdf != pdf_total || \
    actual_files != epub_total + pdf_total || actual_bytes != projected_bytes || \
    unique_inodes != actual_files)); then
    printf 'corpus verification failed: files=%d EPUB=%d PDF=%d bytes=%d inodes=%d\n' \
        "$actual_files" "$actual_epub" "$actual_pdf" "$actual_bytes" "$unique_inodes" >&2
    exit 1
fi

(
    cd "$work_root"
    awk -F '\t' 'NR > 1 { print $6 "  " $4 }' "$generation_plan" \
        | sha256sum --check --quiet -
)

read -r source_manifest_sha ignored < <(sha256sum "$source_manifest")
read -r generation_plan_sha ignored < <(sha256sum "$generation_plan")
read -r corpus_fingerprint ignored < <(
    printf '%s\n%s\n' "$source_manifest_sha" "$generation_plan_sha" | sha256sum
)
manifest_temporary="$corpus_manifest.partial"
if [[ -L $manifest_temporary ]]; then
    printf 'refusing symlink corpus manifest: %s\n' "$manifest_temporary" >&2
    exit 1
fi
{
    printf '{\n'
    printf '  "schema_version": 1,\n'
    printf '  "recipe": "import-corpus-v1",\n'
    printf '  "files": %d,\n' "$actual_files"
    printf '  "epub_files": %d,\n' "$actual_epub"
    printf '  "pdf_files": %d,\n' "$actual_pdf"
    printf '  "logical_bytes": %d,\n' "$actual_bytes"
    printf '  "source_manifest_sha256": "%s",\n' "$source_manifest_sha"
    printf '  "generation_plan_sha256": "%s",\n' "$generation_plan_sha"
    printf '  "corpus_fingerprint_sha256": "%s",\n' "$corpus_fingerprint"
    printf '  "copy_method": "cp --reflink=never",\n'
    printf '  "target_limit_bytes": %d,\n' "$target_limit_bytes"
    printf '  "hard_limit_bytes": %d,\n' "$hard_limit_bytes"
    printf '  "free_space_reserve_bytes": %d\n' "$free_space_reserve_bytes"
    printf '}\n'
} >"$manifest_temporary"
mv -- "$manifest_temporary" "$corpus_manifest"

printf 'corpus_ready files=%d epub=%d pdf=%d bytes=%d fingerprint=%s\n' \
    "$actual_files" "$actual_epub" "$actual_pdf" "$actual_bytes" "$corpus_fingerprint"
du -sh "$corpus_root"
