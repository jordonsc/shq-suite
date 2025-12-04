# Proto Generation

To generate the Python gRPC code from the proto file, run:

```bash
pip install grpcio-tools
./generate.sh
```

This will create `voice_pb2.py` and `voice_pb2_grpc.py` from `voice.proto`.

Alternatively, run directly:
```bash
python3 -m grpc_tools.protoc -I. --python_out=. --grpc_python_out=. voice.proto
```
