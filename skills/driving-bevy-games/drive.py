#!/usr/bin/env python3
"""Send JSON-lines commands to a bevy-agent-feedback socket.

Usage: drive.py PROTOCOL_FILE < commands.jsonl

Reads one JSON command per stdin line ("id" optional, auto-assigned),
prints one JSON response per line.
"""
import json
import socket
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    protocol = json.load(handle)
host, port = protocol["socket_addr"].rsplit(":", 1)
sock = socket.create_connection((host, int(port)), timeout=30)
stream = sock.makefile("rw", encoding="utf-8")

for line_number, line in enumerate(sys.stdin, 1):
    line = line.strip()
    if not line:
        continue
    command = json.loads(line)
    command.setdefault("id", line_number)
    stream.write(json.dumps(command) + "\n")
    stream.flush()
    print(stream.readline().strip(), flush=True)
