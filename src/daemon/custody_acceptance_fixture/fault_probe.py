# Independently owned bounded SQLite fault process, cad1085-security.
# Inherited native identity; no Cadence socket, daemon, credentials or normal reader.
import fcntl
import json
import os
import sqlite3
import sys
from pathlib import Path


def emit(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)


def main():
    request = json.loads(sys.stdin.readline(4096))
    path = Path(request["path"])
    assert path.is_absolute() and path.is_file()
    mode = request["mode"]
    conn = None
    fd = None
    if mode == "recovery_shared":
        fd = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
        # Stock SQLite Unix VFS PENDING_BYTE+2, SHARED_SIZE=510.
        # No SQLite open: that would recover the hot journal before the test.
        fcntl.lockf(fd, fcntl.LOCK_SH | fcntl.LOCK_NB, 510, 0x40000002, os.SEEK_SET)
        emit({"ready": True, "pid": os.getpid(), "mode": mode, "native_shared": True})
    else:
        conn = sqlite3.connect(path.as_uri() + ("?mode=ro" if mode == "shared" else "?mode=rw"),
                               uri=True, isolation_level=None, timeout=1)
        journal = conn.execute("PRAGMA journal_mode").fetchone()[0]
        assert journal == "delete"
        if mode == "shared":
            conn.execute("PRAGMA query_only=ON")
            conn.execute("BEGIN")
            conn.execute("SELECT count(*) FROM sqlite_schema").fetchone()
        elif mode == "hot":
            conn.execute("PRAGMA synchronous=EXTRA")
            conn.execute("PRAGMA cache_size=1")
            conn.execute("PRAGMA cache_spill=ON")
            conn.execute("BEGIN IMMEDIATE")
            # Uncommitted physical corruption rolls back through actual SQLite.
            changed = conn.execute("UPDATE records SET ciphertext=zeroblob(65552)").rowcount
            assert changed > 0
            journal_path = Path(str(path) + "-journal")
            header = journal_path.read_bytes()[:8]
            assert header == bytes.fromhex("d9d505f920a163d7"), "no real hot-journal header"
        elif mode == "header":
            # Explicit bounded fault, ONLY while manager physically closed.
            key_id = bytes.fromhex(request["key_id"])
            assert len(key_id) == 16
            conn.execute("BEGIN IMMEDIATE")
            assert conn.execute("UPDATE vault_header SET key_id=? WHERE singleton=1", (key_id,)).rowcount == 1
            conn.execute("COMMIT")
        else:
            raise AssertionError("unknown fault mode")
        emit({"ready": True, "pid": os.getpid(), "mode": mode, "journal": journal,
              "engine": sqlite3.sqlite_version, "in_transaction": conn.in_transaction})
    command = json.loads(sys.stdin.readline(4096))
    assert command == {"release": True}
    if conn is not None:
        if conn.in_transaction:
            conn.execute("ROLLBACK")
        assert not conn.in_transaction
        conn.close()
    if fd is not None:
        fcntl.lockf(fd, fcntl.LOCK_UN, 510, 0x40000002, os.SEEK_SET)
        os.close(fd)
    emit({"released": True, "pid": os.getpid()})


try:
    main()
except Exception as error:
    # Fixed type only. No SQLite text/paths/request/credential payload.
    emit({"fault_setup_error": type(error).__name__})
    sys.exit(2)
