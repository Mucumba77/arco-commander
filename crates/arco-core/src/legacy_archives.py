import datetime
import json
import sys


class Sink:
    def write(self, data):
        return len(data)


def main():
    kind, operation, path, *args = sys.argv[1:]
    output = sys.stdout.buffer
    if operation == "pack":
        from uc2 import UC2Writer
        with open(args[0], encoding="utf-8") as manifest:
            entries = json.load(manifest)
        with UC2Writer(path, level=4) as archive:
            for source, name, directory, size, modified in entries:
                date = datetime.datetime.fromisoformat(modified) if modified else None
                if directory:
                    archive.mkdir(name, date_time=date)
                else:
                    with open(source, "rb") as stream:
                        archive.write_stream(name, stream, size=size, date_time=date)
        return

    if kind == "ace":
        import acefile
        # Read a bounded JSON password through the anonymous pipe, never argv.
        secret = sys.stdin.buffer.read(4097)
        if len(secret) > 4096:
            raise ValueError("password input too long")
        password = json.loads(secret) if secret else None
        if operation == "encrypt":
            import os
            import struct
            import zlib

            def header(stream):
                prefix = stream.read(4)
                if not prefix:
                    return None
                if len(prefix) != 4:
                    raise ValueError("truncated ACE header")
                checksum, length = struct.unpack("<HH", prefix)
                body = bytearray(stream.read(length))
                if len(body) != length or ((~zlib.crc32(body)) & 65535) != checksum:
                    raise ValueError("invalid ACE header")
                return body

            def write_header(stream, body):
                stream.write(struct.pack("<HH", (~zlib.crc32(body)) & 65535, len(body)))
                stream.write(body)

            with open(path, "rb") as source, open(args[0], "xb") as target:
                main_header = header(source)
                if main_header is None or main_header[0] != 0:
                    raise ValueError("ACE main header missing")
                write_header(target, main_header)
                while (body := header(source)) is not None:
                    if len(body) < 31 or body[0] != 1:
                        raise ValueError("unsupported ACE encryption input")
                    flags = struct.unpack_from("<H", body, 1)[0]
                    packed = struct.unpack_from("<I", body, 3)[0]
                    directory = struct.unpack_from("<I", body, 15)[0] & 16
                    if flags & 0x4000:
                        raise ValueError("input is already encrypted")
                    if not directory:
                        struct.pack_into("<H", body, 1, flags | 0x4000)
                        struct.pack_into("<I", body, 3, (packed + 7) & ~7)
                    write_header(target, body)
                    engine = acefile.AceBlowfish(password)
                    previous_l = previous_r = 0
                    remaining = packed
                    while remaining:
                        chunk = source.read(min(65536, remaining))
                        if not chunk:
                            raise ValueError("truncated ACE body")
                        remaining -= len(chunk)
                        if directory:
                            target.write(chunk)
                            continue
                        if remaining == 0:
                            chunk += os.urandom((-len(chunk)) % 8)
                        encrypted = bytearray()
                        for offset in range(0, len(chunk), 8):
                            left, right = struct.unpack_from("<LL", chunk, offset)
                            previous_l, previous_r = engine._bf_encrypt_block(left ^ previous_l, right ^ previous_r)
                            encrypted.extend(struct.pack("<LL", previous_l, previous_r))
                        target.write(encrypted)
                target.flush()
                os.fsync(target.fileno())
            return
        archive = acefile.open(path, search=524288)
        members = archive.getmembers()

        def info(member):
            try:
                name = member.raw_filename.decode("utf-8")
            except UnicodeDecodeError:
                name = member.raw_filename.decode("cp437")
            return name, member.size, member.is_dir(), member.is_enc(), member.datetime

        def copy(member, target):
            for block in archive.readblocks(member, pwd=password):
                target.write(block)
    else:
        from uc2 import UC2File
        archive = UC2File(path)
        members = archive.infolist()

        def info(member):
            return member.filename, member.file_size, member.is_dir(), False, member.date_time

        def copy(member, target):
            archive.copy_to(member, target)

    with archive:
        if operation == "list":
            if len(members) > 100000:
                raise ValueError("too many archive entries")
            entries = []
            for member in members:
                name, size, directory, encrypted, modified = info(member)
                entries.append([name, size, directory, encrypted, modified.isoformat() if modified else None])
            output.write(json.dumps(entries, ensure_ascii=False).encode("utf-8"))
        elif operation == "extract":
            copy(members[int(args[0])], output)
        elif operation == "extract-manifest":
            with open(args[0], encoding="utf-8") as manifest:
                requested = json.load(manifest)
            if len(requested) > 100000:
                raise ValueError("too many extraction targets")
            for index, target, expected_size in sorted(requested):
                class BoundedTarget:
                    def __init__(self, stream):
                        self.stream = stream
                        self.written = 0
                    def write(self, data):
                        self.written += len(data)
                        if self.written > expected_size:
                            raise ValueError("archive entry exceeds declared size")
                        return self.stream.write(data)
                with open(target, "r+b") as stream:
                    bounded = BoundedTarget(stream)
                    copy(members[index], bounded)
                    if bounded.written != expected_size:
                        raise ValueError("archive entry size mismatch")
                    stream.truncate()
        elif operation == "test":
            if kind == "uc2":
                archive.verify(all_versions=True)
            else:
                for member in members:
                    if not member.is_dir():
                        copy(member, Sink())
        elif operation == "compare-sources":
            import hashlib
            import os

            def key(name):
                return name.replace("\\", "/").rstrip("/").lower()

            with open(args[0], encoding="utf-8") as manifest:
                source_entries = json.load(manifest)
            expected = {key(name): (source, directory) for name, source, directory in source_entries}
            if len(expected) != len(members):
                output.write("Der DOS-Satz enthält eine andere Anzahl an Einträgen.".encode())
                return
            for member in members:
                name, size, directory, _, _ = info(member)
                source = expected.pop(key(name), None)
                if source is None or source[1] != directory:
                    output.write(f"Der DOS-Satz enthält einen abweichenden Eintrag: {name}".encode())
                    return
                if not directory:
                    digest = hashlib.sha256()
                    class HashSink:
                        def write(self, data):
                            digest.update(data)
                            return len(data)
                    copy(member, HashSink())
                    with open(source[0], "rb") as file:
                        source_digest = hashlib.sha256()
                        for block in iter(lambda: file.read(65536), b""):
                            source_digest.update(block)
                        equal = os.fstat(file.fileno()).st_size == size and (
                            source_digest.digest() == digest.digest()
                        )
                    if not equal:
                        output.write(f"Der DOS-Satz enthält abweichende Dateidaten: {name}".encode())
                        return
        else:
            raise ValueError("unknown archive operation")


try:
    main()
except ImportError:
    sys.exit(20)
