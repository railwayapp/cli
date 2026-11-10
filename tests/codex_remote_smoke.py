"""Opt-in live test: python3 tests/codex_remote_smoke.py <connection.json>.

Requires websocket-client. The connection file comes from railway code --codex
--connection-json. Runs one model turn on that agent and retains its thread.
"""
import json
from pathlib import Path
import sys
import time

import websocket


class Client:
    def __init__(self, connection):
        self.socket = websocket.create_connection(
            connection["url"], timeout=120, suppress_origin=True,
            header={"Authorization": "Bearer " + connection["token"]},
        )
        self.next_id = 0
        self.events = []
        self.call("initialize", {"clientInfo": {"name": "railway_cli_smoke", "version": "1.0.0"}})
        self.socket.send(json.dumps({"method": "initialized"}))

    def call(self, method, params):
        self.next_id += 1
        request_id = self.next_id
        self.socket.send(json.dumps({"id": request_id, "method": method, "params": params}))
        while True:
            message = json.loads(self.socket.recv())
            if message.get("id") == request_id and "method" not in message:
                if "error" in message:
                    raise RuntimeError(f"{method} failed: {message['error']}")
                return message["result"]
            self.events.append(message)

    def close(self):
        self.socket.close()


def main(path):
    connection = json.loads(Path(path).read_text())["connection"]
    try:
        unauthorized = websocket.create_connection(connection["url"], timeout=15, suppress_origin=True)
    except websocket.WebSocketBadStatusException as error:
        assert error.status_code == 401, error.status_code
    else:
        unauthorized.close()
        raise AssertionError("Unauthenticated WebSocket was accepted")

    client = Client(connection)
    command = client.call("command/exec", {
        "command": ["python3", "-c", "import json,os,platform; print(json.dumps({'cwd':os.getcwd(),'os':platform.system()}))"],
        "cwd": connection["directory"], "timeoutMs": 10000,
    })
    assert command["exitCode"] == 0, command
    execution = json.loads(command["stdout"])
    assert execution == {"cwd": connection["directory"], "os": "Linux"}, execution
    thread = client.call("thread/start", {"cwd": connection["directory"]})["thread"]
    thread_id = thread["id"]
    client.call("thread/name/set", {"threadId": thread_id, "name": "Railway remote client smoke test"})
    turn = client.call("turn/start", {
        "threadId": thread_id,
        "input": [{"type": "text", "text": "Run pwd once using your shell tool, then reply RAILWAY_CODEX_REMOTE_OK followed by the directory. This is a connection smoke test."}],
    })["turn"]
    # Exercise a network disconnect while the agent is working, then subscribe
    # to the same thread again. No interrupt or server shutdown is requested.
    client.close()
    client = Client(connection)
    resumed = client.call("thread/resume", {"threadId": thread_id})["thread"]
    assert resumed["id"] == thread_id
    deadline = time.monotonic() + 150
    completed = any(t["id"] == turn["id"] and t.get("status") == "completed" for t in resumed.get("turns", []))
    while not completed and time.monotonic() < deadline:
        message = client.events.pop(0) if client.events else json.loads(client.socket.recv())
        if message.get("method") == "turn/completed" and message["params"]["threadId"] == thread_id:
            assert message["params"]["turn"]["status"] == "completed", message
            completed = True
        elif "id" in message and "method" in message:
            raise RuntimeError(f"Unexpected interactive request: {message['method']}")
    assert completed, "The model turn did not complete after reconnect"
    history = client.call("thread/read", {"threadId": thread_id, "includeTurns": True})["thread"]
    items = [item for t in history["turns"] for item in t.get("items", [])]
    assert any(item.get("type") == "commandExecution" for item in items), "No model tool execution recorded"
    assert "RAILWAY_CODEX_REMOTE_OK" in json.dumps(items), "Missing model completion marker"
    client.close()
    print(json.dumps({"authenticated": True, "execution": execution, "threadId": thread_id,
                      "resumedDuringTurn": True, "completed": True, "itemTypes": [i["type"] for i in items]}, indent=2))


if __name__ == "__main__":
    main(sys.argv[1])
