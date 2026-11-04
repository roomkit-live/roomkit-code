"""Local MCP fixture: no network, package download or credentials required."""
import json
import os
from pathlib import Path
import sys

mode, pid_path, trace_path = sys.argv[1:]
Path(pid_path).write_text(str(os.getpid()))
for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "notifications/cancelled":
        with open(trace_path, "a") as trace:
            trace.write("cancelled\n")
        continue
    if "id" not in request:
        continue
    if method == "initialize":
        if mode == "hang":
            continue
        if mode == "fail":
            print("FIXTURE-PRIVATE-CREDENTIAL", file=sys.stderr, flush=True)
            sys.exit(1)
        result = {"protocolVersion": request["params"]["protocolVersion"], "capabilities": {"tools": {}},
                  "serverInfo": {"name": "fixture", "version": "1"},
                  "instructions": "MCP-SERVER-INSTRUCTIONS: use the selected fixture tools."}
    elif method == "tools/list":
        names = [f"tool-{i:03}" for i in range(75)] if mode == "large" else ["read", "danger", "slow"]
        schema = {"type": "object", "properties": {}}
        if mode == "heavy":
            schema["properties"]["note"] = {"type": "string", "description": "A schema that fills the window. " * 450}
        result = {"tools": [{"name": name, "description": "Fixture tool with observable effects",
                             "inputSchema": schema} for name in names]}
    elif method == "tools/call":
        name = request["params"]["name"]
        with open(trace_path, "a") as trace:
            trace.write(name + "\n")
        if name == "slow":
            continue
        if mode == "crash":
            sys.exit(0)
        result = {"content": [{"type": "text", "text": "Fixture MCP result for " + name}]}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
