#!/bin/bash

set -euo pipefail

cd rustdar-platform
wasm-pack build --target web --out-dir ../web/web-pack --no-opt
