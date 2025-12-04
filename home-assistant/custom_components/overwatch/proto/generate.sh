#!/bin/bash
# Generate Python gRPC code from proto file

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Use grpcio-tools 1.72.1 to match Home Assistant's gRPC version
echo "Generating proto files with grpcio-tools==1.72.1..."
python3 -m pip install --quiet grpcio-tools==1.72.1

python3 -m grpc_tools.protoc \
    -I. \
    --python_out=. \
    --grpc_python_out=. \
    voice.proto

# Fix the import to use relative import for Home Assistant package structure
python3 fix_imports.py

echo "Done! Generated voice_pb2.py and voice_pb2_grpc.py"
