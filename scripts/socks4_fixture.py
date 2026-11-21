"""Protocol decoding for the loopback-only SOCKS4 reference peers."""

import socket


def read_connect(connection):
    def field(stream):
        value = bytearray()
        while len(value) < 4096:
            byte = stream.read(1)
            if byte == b"\0":
                return bytes(value)
            if not byte:
                break
            value.extend(byte)
        raise ValueError("Invalid SOCKS4 string")

    with connection.makefile("rb") as stream:
        header = stream.read(8)
        if len(header) != 8 or header[:2] != b"\x04\x01":
            raise ValueError("Expected SOCKS4 CONNECT")
        user = field(stream)
        address = header[4:8]
        host = field(stream).decode("ascii") if address[:3] == b"\0\0\0" and address[3] != 0 else None
        return socket.inet_ntoa(address), host, int.from_bytes(header[2:4], "big"), user


def reply_code(allowed):
    return b"\x00\x5a" + bytes(6) if allowed else b"\x00\x5b" + bytes(6)
