"""Test helper for a local SSH proxy riding on stdin and stdout."""

import os
from pathlib import Path
import socket
import sys
import threading
import time


def relay(host, port, user, password, escaped, empty, expected_host):
    assert host == expected_host
    assert user == 'gate: "Benutzer"'
    assert password == 'paß"%host\\only'
    assert escaped == "line\nvalue %"
    assert empty == ""
    pipe(host, port, expected_host)


def pipe(host, port, expected_host):
    assert host == expected_host
    with socket.create_connection(("127.0.0.1", int(port)), timeout=5) as target:
        target.settimeout(None)

        def send():
            try:
                while data := sys.stdin.buffer.read1(32768):
                    target.sendall(data)
                target.shutdown(socket.SHUT_WR)
            except OSError:
                pass

        threading.Thread(target=send, daemon=True).start()
        while data := target.recv(32768):
            sys.stdout.buffer.write(data)
            sys.stdout.buffer.flush()


def stall(marker):
    Path(marker).write_text(str(os.getpid()), encoding="ascii")
    sys.stdout.buffer.write(b"SSH-2.0-ArcoProxyStall\r\n")
    sys.stdout.buffer.flush()
    time.sleep(30)


if __name__ == "__main__":
    if os.name == "nt":
        import msvcrt

        msvcrt.setmode(sys.stdin.fileno(), os.O_BINARY)
        msvcrt.setmode(sys.stdout.fileno(), os.O_BINARY)
    mode, *args = sys.argv[1:]
    {"relay": relay, "pipe": pipe, "stall": stall}[mode](*args)
