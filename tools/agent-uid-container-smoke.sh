#!/usr/bin/env bash
# CAD-513 physical SO_PEERCRED fixture. Runs only in a disposable, networkless
# container; it never provisions the host or connects to the installed daemon.
set -euo pipefail

binary="${1:-target/debug/cadence}"
test -x "$binary"
binary="$(realpath "$binary")"

docker run --rm -i --network none --mount "type=bind,src=$binary,dst=/opt/cadence,readonly" \
  ubuntu:24.04 bash -euo pipefail -s <<'CONTAINER'
groupadd -g 2200 cadence
useradd -u 1100 -g cadence -M -d /tmp/operator cadence-operator
useradd -u 2200 -g cadence -M -d /tmp/agent cadence-agent
useradd -u 3300 -g cadence -M -d /tmp/outsider outsider
mkdir -p /tmp/operator /tmp/agent /tmp/outsider /tmp/state /tmp/pm /var/lib/cadence
chown 1100:2200 /tmp/operator /tmp/state /var/lib/cadence
chown 2200:2200 /tmp/agent
chown 3300:2200 /tmp/outsider
chmod 0700 /tmp/state /tmp/operator
chmod 0750 /var/lib/cadence
printf '{"uid":2200}\n' >/tmp/state/agent-uid.json
chown 1100:2200 /tmp/state/agent-uid.json
chmod 0600 /tmp/state/agent-uid.json
runuser -u cadence-operator -- env HOME=/tmp/operator CADENCE_PM_DIR=/tmp/pm \
  /opt/cadence daemon --state-dir /tmp/state run >/tmp/daemon.log 2>&1 &
daemon_pid=$!
trap 'kill "$daemon_pid" 2>/dev/null || true' EXIT
for n in $(seq 1 100); do
  test -S /tmp/state/cadence.sock && test -S /var/lib/cadence/cadence.sock && break
  sleep 0.1
done
if ! test -S /tmp/state/cadence.sock || ! test -S /var/lib/cadence/cadence.sock; then
  cat /tmp/daemon.log
  exit 1
fi
test "$(stat -c %a /var/lib/cadence/cadence.sock)" = 660
# Make both fixture paths traversable and both sockets connectable to the
# third UID. This proves the daemon admit set itself, beyond filesystem ACLs.
chmod 0711 /tmp/state
chmod 0777 /tmp/state/cadence.sock /var/lib/cadence/cadence.sock
probe='use IO::Socket::UNIX; my $s=IO::Socket::UNIX->new(Type=>1,Peer=>$ARGV[0]) or die "connect: $!"; my $m=$ARGV[1] // "health"; print $s "{\"method\":\"$m\",\"params\":{\"peer_uid\":1100,\"caller\":\"operator\",\"on\":false}}\n"; my $r=<$s>; defined($r) or die "closed by peer"; print $r;'
for socket in /tmp/state/cadence.sock /var/lib/cadence/cadence.sock; do
  runuser -u cadence-operator -- perl -e "$probe" "$socket" | grep -q '"ok":true'
  # `setsid` makes the exact admitted agent UID a detached caller.
  runuser -u cadence-agent -- setsid perl -e "$probe" "$socket" | grep -q '"ok":true'
  runuser -u cadence-agent -- setsid perl -e "$probe" "$socket" update_drain | grep -q '"ok":false'
  if runuser -u outsider -- setsid perl -e "$probe" "$socket" >/tmp/outsider.out 2>/tmp/outsider.err; then
    echo "unlisted UID received an RPC response on $socket" >&2
    exit 1
  fi
  echo "admit set verified on $socket"
done
mkdir -p /tmp/ui
printf '<!doctype html><title>fixture</title>\n' >/tmp/ui/index.html
runuser -u cadence-operator -- env HOME=/tmp/operator CADENCE_PM_DIR=/tmp/pm \
  /opt/cadence ui run --state-dir /tmp/state --port 3115 --dist /tmp/ui >/tmp/ui.log 2>&1 &
ui_pid=$!
trap 'kill "$ui_pid" "$daemon_pid" 2>/dev/null || true' EXIT
for n in $(seq 1 100); do
  if perl -MIO::Socket::INET -e 'exit(IO::Socket::INET->new(PeerAddr=>"127.0.0.1",PeerPort=>3115,Proto=>"tcp") ? 0 : 1)'; then
    break
  fi
  sleep 0.1
done
if ! kill -0 "$ui_pid" 2>/dev/null; then cat /tmp/ui.log; exit 1; fi
agent_link=$(runuser -u cadence-operator -- env HOME=/tmp/operator \
  /opt/cadence ui login --state-dir /tmp/state --port 3115 --json |
  perl -0777 -ne 'print $1 if /"link"\s*:\s*"([^"]+)"/')
http_probe='use IO::Socket::INET; my ($url,$header)=@ARGV; $url =~ m{^http://([^/]+)/login#n=([^&]+)$} or die "bad link"; my ($host,$nonce)=($1,$2); my $body="{\"nonce\":\"$nonce\"}"; my $extra=$header ne "" ? "$header\r\n" : ""; my $s=IO::Socket::INET->new(PeerAddr=>"127.0.0.1",PeerPort=>3115,Proto=>"tcp") or die "connect: $!"; print $s "POST /api/session HTTP/1.1\r\nHost: $host\r\nOrigin: http://$host\r\nContent-Type: application/json\r\nContent-Length: ".length($body)."\r\nX-Cadence-Board: 1\r\nX-Cadence-Caller: operator\r\n${extra}Connection: close\r\n\r\n$body"; my $status=<$s>; print $status;'
agent_status=$(runuser -u cadence-agent -- setsid perl -e "$http_probe" "$agent_link" \
  'X-Forwarded-User: operator')
operator_link=$(runuser -u cadence-operator -- env HOME=/tmp/operator \
  /opt/cadence ui login --state-dir /tmp/state --port 3115 --json |
  perl -0777 -ne 'print $1 if /"link"\s*:\s*"([^"]+)"/')
operator_status=$(runuser -u cadence-operator -- perl -e "$http_probe" "$operator_link" '')
echo "agent login attempt: $agent_status; operator login attempt: $operator_status"
grep -Eq '^HTTP/1\.[01] 40[13] ' <<<"$agent_status"
grep -Eq '^HTTP/1\.[01] 200 ' <<<"$operator_status"
echo 'board login link refused to detached agent UID despite forged headers'
CONTAINER
