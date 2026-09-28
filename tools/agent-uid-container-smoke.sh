#!/usr/bin/env bash
# CAD-513 physical SO_PEERCRED fixture. Runs only in a disposable, networkless
# container; it never provisions the host or connects to the installed daemon.
set -euo pipefail

binary="${1:-target/debug/cadence}"
test -x "$binary"
binary="$(realpath "$binary")"
http_probe="$(realpath "$(dirname "$0")/agent-uid-http-probe.pl")"

docker run --rm -i --network none --mount "type=bind,src=$binary,dst=/opt/cadence,readonly" \
  --mount "type=bind,src=$http_probe,dst=/opt/http-probe.pl,readonly" \
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
test "$(stat -c %u:%a /tmp/state/agent-uid-mode.json)" = 1100:600
test "$(stat -c %a /var/lib/cadence/cadence.sock)" = 660
# Make both fixture paths traversable and both sockets connectable to the
# third UID. This proves the daemon admit set itself, beyond filesystem ACLs.
chmod 0711 /tmp/state
chmod 0777 /tmp/state/cadence.sock /var/lib/cadence/cadence.sock
if runuser -u cadence-agent -- perl -e 'exit(unlink("/tmp/state/agent-uid-mode.json") ? 0 : 1)'; then
  echo 'agent UID removed persistent mode marker' >&2
  exit 1
fi
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
setpriv --reuid=1100 --regid=2200 --clear-groups env HOME=/tmp/operator CADENCE_PM_DIR=/tmp/pm \
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
agent_status=$(runuser -u cadence-agent -- setsid perl /opt/http-probe.pl login "$agent_link" \
  'X-Forwarded-User: operator')
operator_link=$(runuser -u cadence-operator -- env HOME=/tmp/operator \
  /opt/cadence ui login --state-dir /tmp/state --port 3115 --json |
  perl -0777 -ne 'print $1 if /"link"\s*:\s*"([^"]+)"/')
operator_status=$(runuser -u cadence-operator -- perl /opt/http-probe.pl \
  login "$operator_link" '' /tmp/operator-session)
echo "agent login attempt: $agent_status; operator login attempt: $operator_status"
grep -Eq '^HTTP/1\.[01] 40[13] ' <<<"$agent_status"
grep -Eq '^HTTP/1\.[01] 200 ' <<<"$operator_status"
echo 'board login link refused to detached agent UID despite forged headers'
mapfile -t operator_session </tmp/operator-session
test "${#operator_session[@]}" -eq 2
# The daemon keeps UID 2200 pinned for this boot. Removing the private
# record must not let the board fall back to a session-bearing NoAgent.
mv /tmp/state/agent-uid.json /tmp/agent-uid.saved
removed_link=$(runuser -u cadence-operator -- env HOME=/tmp/operator \
  /opt/cadence ui login --state-dir /tmp/state --port 3115 --json |
  perl -0777 -ne 'print $1 if /"link"\s*:\s*"([^"]+)"/')
removed_status=$(runuser -u cadence-agent -- setsid perl /opt/http-probe.pl login "$removed_link" '')
grep -Eq '^HTTP/1\.[01] 403 ' <<<"$removed_status"
# A valid operator cookie and page key remain live across a board restart.
# An old agent UID presenting both must still fail after the record vanishes.
kill "$ui_pid"
wait "$ui_pid" 2>/dev/null || true
setpriv --reuid=1100 --regid=2200 --clear-groups env HOME=/tmp/operator CADENCE_PM_DIR=/tmp/pm \
  /opt/cadence ui run --state-dir /tmp/state --port 3115 --dist /tmp/ui >/tmp/ui-restart-normal.log 2>&1 &
ui_pid=$!
for n in $(seq 1 100); do
  if perl -MIO::Socket::INET -e 'exit(IO::Socket::INET->new(PeerAddr=>"127.0.0.1",PeerPort=>3115,Proto=>"tcp") ? 0 : 1)'; then break; fi
  sleep 0.1
done
if ! kill -0 "$ui_pid" 2>/dev/null; then cat /tmp/ui-restart-normal.log; exit 1; fi
stolen_restart_status=$(runuser -u cadence-agent -- setsid perl /opt/http-probe.pl \
  write "${operator_session[0]}" "${operator_session[1]}")
grep -Eq '^HTTP/1\.[01] 403 ' <<<"$stolen_restart_status"
# Restart the board with an inherited forged CADENCE_SOCKET. The fake RPC
# endpoint reports whichever UID matches the edited record and returns a
# synthetic successful login. Security attribution must still query the
# actual daemon on the private state socket, so the agent gets 403.
kill "$ui_pid"
wait "$ui_pid" 2>/dev/null || true
printf 'null\n' >/tmp/fake-health-uid
fake_server='use IO::Socket::UNIX; my $path="/tmp/fake.sock"; unlink $path; my $server=IO::Socket::UNIX->new(Type=>1,Local=>$path,Listen=>16) or die "bind: $!"; chmod 0777,$path; while(my $client=$server->accept()) { my $request=<$client>; my ($method)=$request =~ /"method":"([^"]+)"/; open my $log,">>","/tmp/fake-methods" or die $!; print $log "$method\n"; close $log; open my $f,"<","/tmp/fake-health-uid" or die $!; my $uid=<$f>; chomp $uid; close $f; my $result=$method eq "health" ? "{\"agent_uid\":$uid}" : "{\"token\":\"fake\",\"session\":{\"expires_at\":9999999999},\"key\":\"fake\"}"; print $client "{\"ok\":true,\"result\":$result}\n"; close $client; }'
perl -e "$fake_server" >/tmp/fake.log 2>&1 &
fake_pid=$!
for n in $(seq 1 100); do test -S /tmp/fake.sock && break; sleep 0.1; done
test -S /tmp/fake.sock
setpriv --reuid=1100 --regid=2200 --clear-groups env HOME=/tmp/operator \
  CADENCE_PM_DIR=/tmp/pm CADENCE_SOCKET=/tmp/fake.sock \
  /opt/cadence ui run --state-dir /tmp/state --port 3115 --dist /tmp/ui >/tmp/ui-restart.log 2>&1 &
ui_pid=$!
trap 'kill "$fake_pid" "$ui_pid" "$daemon_pid" 2>/dev/null || true' EXIT
for n in $(seq 1 100); do
  if perl -MIO::Socket::INET -e 'exit(IO::Socket::INET->new(PeerAddr=>"127.0.0.1",PeerPort=>3115,Proto=>"tcp") ? 0 : 1)'; then break; fi
  sleep 0.1
done
if ! kill -0 "$ui_pid" 2>/dev/null; then cat /tmp/ui-restart.log; exit 1; fi
: >/tmp/fake-methods
spoof_link=$(runuser -u cadence-operator -- env HOME=/tmp/operator \
  /opt/cadence ui login --state-dir /tmp/state --port 3115 --json |
  perl -0777 -ne 'print $1 if /"link"\s*:\s*"([^"]+)"/')
spoof_status=$(runuser -u cadence-agent -- setsid perl /opt/http-probe.pl login "$spoof_link" '')
grep -Eq '^HTTP/1\.[01] 403 ' <<<"$spoof_status"
# A new valid NSS UID and matching private record also must not upgrade
# an old UID 2200 process before the daemon itself is restarted.
usermod -u 4400 cadence-agent
printf '{"uid":4400}\n' >/tmp/state/agent-uid.json
chown 1100:2200 /tmp/state/agent-uid.json
chmod 0600 /tmp/state/agent-uid.json
printf '4400\n' >/tmp/fake-health-uid
changed_link=$(runuser -u cadence-operator -- env HOME=/tmp/operator \
  /opt/cadence ui login --state-dir /tmp/state --port 3115 --json |
  perl -0777 -ne 'print $1 if /"link"\s*:\s*"([^"]+)"/')
changed_status=$(setpriv --reuid=2200 --regid=2200 --clear-groups setsid \
  perl /opt/http-probe.pl login "$changed_link" '')
grep -Eq '^HTTP/1\.[01] 403 ' <<<"$changed_status"
grep -q '^operator_session_open$' /tmp/fake-methods
if grep -q '^health$' /tmp/fake-methods; then
  echo 'board health proof followed forged CADENCE_SOCKET' >&2
  exit 1
fi
echo "board record removal: $removed_status; stolen session after restart: $stolen_restart_status; spoofed restart: $spoof_status; valid UID drift: $changed_status"
# The record is now absent and the private daemon is gone. A board restart
# must consult the durable mode marker and refuse to become a standalone
# legacy board while the old agent process could still hold a session.
kill "$ui_pid" "$daemon_pid"
wait "$ui_pid" 2>/dev/null || true
wait "$daemon_pid" 2>/dev/null || true
rm /tmp/state/agent-uid.json
if runuser -u cadence-operator -- env HOME=/tmp/operator CADENCE_PM_DIR=/tmp/pm \
  timeout 5 /opt/cadence ui run --state-dir /tmp/state --port 3115 --dist /tmp/ui \
  >/tmp/ui-offline.log 2>&1; then
  echo 'board restarted in legacy mode after UID record and daemon loss' >&2
  exit 1
fi
grep -q 'Agent UID mode has no private daemon boot pin' /tmp/ui-offline.log
if runuser -u cadence-operator -- env HOME=/tmp/operator CADENCE_PM_DIR=/tmp/pm \
  timeout 5 /opt/cadence daemon --state-dir /tmp/state run >/tmp/daemon-downgrade.log 2>&1; then
  echo 'daemon restarted in legacy mode after UID record loss' >&2
  exit 1
fi
grep -q 'agent UID mode marker requires its original configured UID' /tmp/daemon-downgrade.log
printf '{"uid":2200,"forged":true}\n' >/tmp/state/agent-uid-mode.json
if runuser -u cadence-operator -- env HOME=/tmp/operator CADENCE_PM_DIR=/tmp/pm \
  timeout 5 /opt/cadence ui run --state-dir /tmp/state --port 3115 --dist /tmp/ui \
  >/tmp/ui-forged-marker.log 2>&1; then
  echo 'board accepted forged mode marker' >&2
  exit 1
fi
grep -q 'agent UID mode marker is malformed' /tmp/ui-forged-marker.log
echo 'historical UID mode refuses standalone board restart after record and daemon loss'
CONTAINER
