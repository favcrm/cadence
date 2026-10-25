#!/usr/bin/env python3
"""Owned native client: real sockets/process/session; no identity overrides."""
import json
import os
import socket
import sys

LIMIT = 512 * 1024
if len(sys.argv) != 2 or sys.argv[1] not in ('attached', 'setsid'):
    raise SystemExit(2)
if sys.argv[1] == 'setsid':
    os.setsid()
# Inherit ALL identity variables. This client never consults CADENCE_SOCKET,
# CADENCE_TEST_AS, a seam token or installed cadence; supplied socket is private.
print(json.dumps({'ready': True, 'pid': os.getpid(), 'sid': os.getsid(0)}), flush=True)

def receive(sock, line=False):
    result = bytearray()
    while len(result) <= LIMIT:
        part = sock.recv(min(16384, LIMIT + 1 - len(result)))
        if not part:
            break
        result.extend(part)
        if line and b'\n' in result:
            break
    if len(result) > LIMIT or (line and b'\n' not in result):
        raise RuntimeError('bounded response failed')
    return bytes(result)

for line in sys.stdin:
    try:
        if len(line) > LIMIT:
            raise RuntimeError('bounded input failed')
        item = json.loads(line)
        if item['transport'] == 'stop':
            print(json.dumps({'stopped': True}), flush=True)
            break
        if item['transport'] == 'rpc':
            request = {'method': item['method'], 'params': item['params']}
            assert 'test_caller' not in request
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
                sock.settimeout(10)
                sock.connect(item['socket'])
                sock.sendall(json.dumps(request).encode() + b'\n')
                answer = json.loads(receive(sock, True).split(b'\n', 1)[0])
            print(json.dumps({'rpc': answer}), flush=True)
        elif item['transport'] == 'http':
            body = json.dumps(item['body']).encode() if item['verb'] != 'GET' else b''
            host = item['host']
            headers = [f"{item['verb']} {item['path']} HTTP/1.0", f'Host: {host}',
                       'Content-Type: application/json', 'X-Cadence-Board: 1',
                       'Sec-Fetch-Site: same-origin', f'Origin: http://{host}',
                       f"Cookie: cadence_operator_{item['port']}={item['token']}",
                       f"X-Cadence-Session: {item['key']}",
                       f'Content-Length: {len(body)}', 'Connection: close']
            # No X-Cadence-Test-* header, no synthetic actor or bearer fallback.
            with socket.create_connection(('127.0.0.1', item['port']), timeout=10) as sock:
                sock.settimeout(10)
                sock.sendall(('\r\n'.join(headers) + '\r\n\r\n').encode() + body)
                answer = receive(sock)
            head, payload = answer.split(b'\r\n\r\n', 1)
            status = int(head.split(b'\r\n', 1)[0].split()[1])
            print(json.dumps({'http': {'status': status,
                                      'body': payload.decode('utf-8', 'strict')}}), flush=True)
        else:
            raise RuntimeError('unknown transport')
    except Exception:
        # Setup/transport errors are never converted into an authority refusal.
        print(json.dumps({'probe_error': True}), flush=True)
        raise SystemExit(70)
