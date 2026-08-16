#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
script_path="$(realpath -e -- "${BASH_SOURCE[0]}")"
python_bin="${K3_PYTHON:-$script_dir/.venv-separator/bin/python}"

# K3 starts this script again as its JSON-lines separation worker. Keeping the
# launcher here avoids relying on the virtualenv entry point's absolute shebang.
if [[ "${K3_SEPARATE_WORKER_MODE:-}" == "1" ]]; then
    if [[ ! -x "$python_bin" ]]; then
        echo "k3-separate: Python environment not found: $python_bin" >&2
        exit 1
    fi
    export PYTHONPATH="$script_dir/python/separator/src${PYTHONPATH:+:$PYTHONPATH}"
    exec "$python_bin" -m k3_separator "$@"
fi

default_output_dir="${K3_OUTPUT_DIR:-/mnt/h/CloudMusic/k3}"

usage() {
    echo "usage: $0 -f AUDIO_FILE... [-d PROJECTS_DIR]" >&2
    echo "default projects directory: $default_output_dir" >&2
    echo "set K3_PRESERVE_BACKING_VOCALS=false to disable keeping backing vocals in accompaniment" >&2
}

input_args=()
output_arg="$default_output_dir"
while [[ $# -gt 0 ]]; do
    case "$1" in
        -f)
            shift
            file_count=0
            while [[ $# -gt 0 && "$1" != -* ]]; do
                input_args+=("$1")
                file_count=$((file_count + 1))
                shift
            done
            if [[ $file_count -eq 0 ]]; then
                echo "k3-separate: option -f requires at least one audio file" >&2
                usage
                exit 2
            fi
            ;;
        -d)
            if [[ $# -lt 2 ]]; then
                echo "k3-separate: option -d requires a value" >&2
                usage
                exit 2
            fi
            output_arg="$2"
            shift 2
            ;;
        -h)
            usage
            exit 0
            ;;
        -*)
            echo "k3-separate: unknown option: $1" >&2
            usage
            exit 2
            ;;
        *)
            echo "k3-separate: unexpected positional argument: $1" >&2
            usage
            exit 2
            ;;
    esac
done

if [[ ${#input_args[@]} -eq 0 ]]; then
    usage
    exit 2
fi

if [[ ! -x "$python_bin" ]]; then
    echo "k3-separate: Python environment not found: $python_bin" >&2
    echo "run: bash python/separator/scripts/install-gpu.sh" >&2
    exit 1
fi

mkdir -p -- "$output_arg"
if ! projects_dir="$(realpath -e -- "$output_arg")" || [[ ! -d "$projects_dir" ]]; then
    echo "k3-separate: projects directory is not accessible: $output_arg" >&2
    exit 1
fi

input_paths=()
project_names=()
project_dirs=()
declare -A seen_projects=()
for input_arg in "${input_args[@]}"; do
    if ! input_path="$(realpath -e -- "$input_arg")" || [[ ! -f "$input_path" ]]; then
        echo "k3-separate: audio file not found: $input_arg" >&2
        exit 1
    fi

    filename="$(basename "$input_path")"
    project_name="${filename%.*}"
    if [[ -z "$project_name" ]]; then
        project_name="$filename"
    fi
    project_dir="$projects_dir/$project_name"

    if [[ -n "${seen_projects[$project_name]+present}" ]]; then
        echo "k3-separate: multiple inputs map to the same project: $project_name" >&2
        exit 1
    fi
    seen_projects["$project_name"]=1
    if [[ -e "$project_dir" && ! -f "$project_dir/project.json" ]]; then
        echo "k3-separate: destination exists but is not a project: $project_dir" >&2
        exit 1
    fi

    input_paths+=("$input_path")
    project_names+=("$project_name")
    project_dirs+=("$project_dir")
done

if [[ -n "${K3_BIN:-}" ]]; then
    k3_bin="$K3_BIN"
elif [[ -x "$script_dir/target/release/k3" ]]; then
    k3_bin="$script_dir/target/release/k3"
elif [[ -x "$script_dir/target/debug/k3" ]]; then
    k3_bin="$script_dir/target/debug/k3"
elif command -v k3 >/dev/null 2>&1; then
    k3_bin="$(command -v k3)"
else
    echo "k3-separate: k3 executable not found; run: cargo build -p k3 --release" >&2
    exit 1
fi

if [[ ! -x "$k3_bin" ]]; then
    echo "k3-separate: k3 executable is not executable: $k3_bin" >&2
    exit 1
fi

profile="${K3_PROFILE:-quality}"
model_id="${K3_MODEL:-}"
case "${K3_AUTOCAST:-true}" in
    1|true|yes|on)
        disable_autocast=0
        ;;
    0|false|no|off)
        disable_autocast=1
        ;;
    *)
        echo "k3-separate: K3_AUTOCAST must be true or false" >&2
        exit 2
        ;;
esac
case "${K3_PRESERVE_BACKING_VOCALS:-true}" in
    1|true|yes|on)
        preserve_backing_vocals=1
        ;;
    0|false|no|off)
        preserve_backing_vocals=0
        ;;
    *)
        echo "k3-separate: K3_PRESERVE_BACKING_VOCALS must be true or false" >&2
        exit 2
        ;;
esac

failures=0
for index in "${!input_paths[@]}"; do
    input_path="${input_paths[$index]}"
    project_name="${project_names[$index]}"
    project_dir="${project_dirs[$index]}"

    replacing=0
    if [[ -f "$project_dir/project.json" ]]; then
        replacing=1
        echo "k3-separate: re-separating existing project with current settings: $project_dir" >&2
    else
        echo "k3-separate: creating project for $input_path" >&2
        if ! "$k3_bin" new \
            --root "$project_dir" \
            --song "$input_path" \
            --title "$project_name"; then
            echo "k3-separate: failed to create project: $project_dir" >&2
            failures=$((failures + 1))
            continue
        fi
    fi

    separate_args=(
        separate
        --project "$project_dir"
        --profile "$profile"
        --worker "$script_path"
    )
    if [[ $replacing -eq 1 ]]; then
        separate_args+=(--overwrite)
    fi
    if [[ -n "$model_id" ]]; then
        separate_args+=(--model "$model_id")
    fi
    if [[ -n "${K3_MODEL_DIR:-}" ]]; then
        separate_args+=(--model-dir "$K3_MODEL_DIR")
    fi
    if [[ -n "${K3_SEGMENT_SIZE:-}" ]]; then
        separate_args+=(--segment-size "$K3_SEGMENT_SIZE")
    fi
    if [[ $disable_autocast -eq 1 ]]; then
        separate_args+=(--no-autocast)
    fi
    if [[ $preserve_backing_vocals -eq 0 ]]; then
        separate_args+=(--no-preserve-backing-vocals)
    fi

    if K3_SEPARATE_WORKER_MODE=1 "$k3_bin" "${separate_args[@]}"; then
        echo "project: $project_dir"
        echo "vocals: $project_dir/stems/vocals.wav"
        if [[ $preserve_backing_vocals -eq 1 ]]; then
            echo "backing vocals: $project_dir/stems/backing-vocals.wav"
        fi
        echo "accompaniment: $project_dir/stems/accompaniment.wav"
    else
        echo "k3-separate: separation failed for project: $project_dir" >&2
        failures=$((failures + 1))
    fi
done

if [[ $failures -ne 0 ]]; then
    echo "k3-separate: $failures project(s) failed" >&2
    exit 1
fi
