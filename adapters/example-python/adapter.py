#!/usr/bin/env python3
"""A Display Lab adapter template, in plain Python with no dependencies.

Copy this to start an adapter for your own app. It speaks protocol 1.0:
JSON-RPC 2.0 on stdin/stdout, one message per line. Log to stderr only;
stdout belongs to the protocol.

This example does not drive a real app. It keeps a slide counter so the
protocol can be exercised end to end. Replace the bodies of launch(),
handle_input(), state() and shutdown() with calls into your app (its API,
its MCP server, keystrokes, whatever it offers), and add bindings() and
expected_frame() if your app can answer them (see docs/PROTOCOL.md).
"""
import json
import sys

PROTOCOL = "1.0"

app = {"running": False, "slide": 1}


def info():
    return {
        "name": "example-python",
        "version": "0.1.0",
        "protocol": PROTOCOL,
        "capabilities": {"state": True},
    }


def launch(params):
    # params["screens"] lists what the OS reports right now. Start your app.
    app["running"] = True
    app["slide"] = 1
    return {}


def handle_input(params):
    action = params.get("action")
    if action == "next":
        app["slide"] += 1
    elif action == "previous":
        app["slide"] = max(1, app["slide"] - 1)
    else:
        raise AppError(f"unknown action {action!r}")
    return {}


def state(_params):
    return {"slide": app["slide"], "running": app["running"]}


def shutdown(_params):
    app["running"] = False
    return {}


class AppError(Exception):
    code = -32002


METHODS = {
    "initialize": lambda p: info(),
    "launch": launch,
    "input": handle_input,
    "state": state,
    "shutdown": shutdown,
}


def reply(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except json.JSONDecodeError as e:
            reply({"jsonrpc": "2.0", "id": None, "error": {"code": -32700, "message": str(e)}})
            continue
        if "id" not in request:
            continue  # a notification; protocol 1 defines none
        method = request.get("method")
        handler = METHODS.get(method)
        if handler is None:
            # Unknown, or a capability this adapter does not offer.
            reply({"jsonrpc": "2.0", "id": request["id"],
                   "error": {"code": -32601, "message": f"no method {method!r}"}})
            continue
        try:
            result = handler(request.get("params") or {})
            reply({"jsonrpc": "2.0", "id": request["id"], "result": result})
        except AppError as e:
            reply({"jsonrpc": "2.0", "id": request["id"], "error": {"code": e.code, "message": str(e)}})
        if method == "shutdown":
            return


if __name__ == "__main__":
    main()
