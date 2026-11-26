# Invoked with an absolute, versioned destination and a pinned revision.
# Build a self-contained executable so another llama.cpp installation can coexist.
set -eu
umask 077
destination=$1
revision=$2
if [ -x "$destination" ]; then exit 0; fi
mkdir -p "$(dirname "$destination")"
# The owning process creates, monitors and removes our private working directory.
git init -q source
git -C source fetch -q --depth=1 https://github.com/OVHai-LLM/llama.cpp.git "$revision"
git -C source checkout -q --detach FETCH_HEAD
test "$(git -C source rev-parse HEAD)" = "$revision"
set -- -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF -DGGML_BACKEND_DL=OFF \
    -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF -DLLAMA_BUILD_SERVER=ON \
    -DLLAMA_USE_PREBUILT_UI=OFF -DLLAMA_BUILD_UI=OFF -DGGML_METAL_EMBED_LIBRARY=ON
if [ "$(uname -s)" = Linux ] && command -v nvcc >/dev/null && command -v nvidia-smi >/dev/null && nvidia-smi -L >/dev/null 2>&1; then
    set -- "$@" -DGGML_CUDA=ON -DCMAKE_CUDA_ARCHITECTURES=native
fi
cmake -S source -B build "$@"
cmake --build build --config Release --target llama-server --parallel 4
build/bin/llama-server --version
# Hard-linking a completed binary publishes atomically and never replaces a file.
ln build/bin/llama-server "$destination" || test -x "$destination"
