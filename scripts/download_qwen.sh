#!/usr/bin/env bash
#
# Downloads a tiny, fast Qwen GGUF for end-to-end testing of Kensho's local
# inference pipeline, into <project-root>/.models/.
#
set -euo pipefail

MODEL_REPO="Qwen/Qwen2.5-0.5B-Instruct-GGUF"
MODEL_FILE="qwen2.5-0.5b-instruct-q4_k_m.gguf"
URL="https://huggingface.co/${MODEL_REPO}/resolve/main/${MODEL_FILE}?download=true"

# Project root is the parent of this script's directory.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
DEST_DIR="${ROOT_DIR}/.models"
DEST="${DEST_DIR}/${MODEL_FILE}"

mkdir -p "${DEST_DIR}"

if [[ -f "${DEST}" ]]; then
  echo "[ok] modelo já existe: ${DEST}"
else
  echo "[..] baixando ${MODEL_FILE} (~400 MB) de HuggingFace..."
  if command -v curl >/dev/null 2>&1; then
    curl -L --fail --progress-bar -o "${DEST}.part" "${URL}"
  elif command -v wget >/dev/null 2>&1; then
    wget --show-progress -O "${DEST}.part" "${URL}"
  else
    echo "[erro] é necessário curl ou wget instalado." >&2
    exit 1
  fi
  mv "${DEST}.part" "${DEST}"
  echo "[ok] salvo em ${DEST}"
fi

echo
echo "Exporte o caminho do modelo:"
echo "  export KENSHO_MODEL_PATH=${DEST}"
echo
echo "Depois rode (com o motor real):"
echo "  npm run tauri dev -- --features llama"
