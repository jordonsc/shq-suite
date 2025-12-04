#!/usr/bin/env python3
"""Post-process generated gRPC files for Home Assistant compatibility."""

from pathlib import Path


def fix_grpc_file():
    """Fix voice_pb2_grpc.py for Home Assistant compatibility."""
    grpc_file = Path(__file__).parent / "voice_pb2_grpc.py"

    if not grpc_file.exists():
        print(f"Error: {grpc_file} not found")
        return

    content = grpc_file.read_text()

    # Change absolute import to relative import for Home Assistant package structure
    content = content.replace(
        "import voice_pb2 as voice__pb2",
        "from . import voice_pb2 as voice__pb2"
    )

    # Write back
    grpc_file.write_text(content)
    print(f"Fixed imports in {grpc_file.name}")


if __name__ == "__main__":
    fix_grpc_file()
