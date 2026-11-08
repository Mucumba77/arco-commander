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
        archive = acefile.open(path, search=0)
        if archive.is_multivolume():
            archive.close()
            sys.exit(21)
        members = archive.getmembers()

        def info(member):
            try:
                name = member.raw_filename.decode("utf-8")
            except UnicodeDecodeError:
                name = member.raw_filename.decode("cp437")
            return name, member.size, member.is_dir(), member.is_enc(), member.datetime

        def copy(member, target):
            for block in archive.readblocks(member):
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
