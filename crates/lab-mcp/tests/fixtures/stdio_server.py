"""A minimal MCP server on stdio, for the transport test. Standard library only.

It logs a non-JSON line to stdout and sends a ping request before its first
reply, to check the client skips the one and refuses the other.
"""
import json
import sys

slide = 1
pinged = False


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def text(obj, error=False):
    return {"content": [{"type": "text", "text": json.dumps(obj)}], "isError": error}


for line in sys.stdin:
    msg = json.loads(line)
    if "id" not in msg:
        continue  # a notification
    if "method" not in msg:
        continue  # the client's answer to our ping
    if not pinged:
        pinged = True
        print("server starting up")
        send({"jsonrpc": "2.0", "id": "ping-1", "method": "ping"})
    method, params = msg["method"], msg.get("params", {})
    if method == "initialize":
        result = {"protocolVersion": params["protocolVersion"], "capabilities": {"tools": {}},
                  "serverInfo": {"name": "stdio-fixture", "version": "1"}}
    elif method == "tools/call":
        name = params["name"]
        if name == "next":
            slide += 1
            result = text({})
        elif name == "state":
            result = text({"slide": slide})
        else:
            result = text({"error": "Unknown tool: " + name}, error=True)
    else:
        send({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32601, "message": method}})
        continue
    send({"jsonrpc": "2.0", "id": msg["id"], "result": result})
