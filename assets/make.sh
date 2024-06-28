#!/bin/bash

set -eux

cd "$(dirname "${BASH_SOURCE[0]}")"

for resolution in 16 22 32 48 64 256 512; do
  mkdir -p \
    "icon-generated/${resolution}x${resolution}" \
    "icon-generated/${resolution}x${resolution}@2"
  inkscape -w "${resolution}" --export-background-opacity=0 \
    --export-filename="icon-generated/${resolution}x${resolution}/icon.png" \
    'icon.svg'
  inkscape -w "$((resolution * 2))" --export-background-opacity=0 \
    --export-filename="icon-generated/${resolution}x${resolution}@2/icon.png" \
    'icon.svg'
done
