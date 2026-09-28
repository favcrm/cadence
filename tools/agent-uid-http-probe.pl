#!/usr/bin/env perl
# Disposable CAD-513 container fixture only. No external network use.
use strict;
use warnings;
use IO::Socket::INET;

sub exchange {
    my ($path, $host, $body, $headers) = @_;
    my $socket = IO::Socket::INET->new(
        PeerAddr => '127.0.0.1', PeerPort => 3115, Proto => 'tcp'
    ) or die "board connect: $!";
    print {$socket} "POST $path HTTP/1.1\r\n",
        "Host: $host\r\nOrigin: http://$host\r\n",
        "Content-Type: application/json\r\n",
        'Content-Length: ', length($body), "\r\n",
        "X-Cadence-Board: 1\r\n", $headers,
        "Connection: close\r\n\r\n", $body;

    # Read exactly through the header terminator. Avoid waiting for EOF:
    # tiny_http may keep a connection alive after answering one request.
    my $head = '';
    while (index($head, "\r\n\r\n") < 0) {
        my $read = sysread($socket, my $byte, 1);
        die 'board closed before headers' unless $read;
        $head .= $byte;
        die 'oversized board headers' if length($head) > 65536;
    }
    my ($status) = $head =~ /\A([^\r\n]+)/;
    my ($length) = $head =~ /^Content-Length:\s*(\d+)/im;
    die 'board response lacks Content-Length' unless defined $length;
    my $response = '';
    while (length($response) < $length) {
        my $read = sysread($socket, my $chunk, $length - length($response));
        die 'board closed before response body' unless $read;
        $response .= $chunk;
    }
    return ($status, $head, $response);
}

my $mode = shift @ARGV // die 'mode required';
my $host = 'cadence-3115.localhost:3115';
if ($mode eq 'login') {
    my ($link, $extra, $capture) = @ARGV;
    $link =~ m{\Ahttp://([^/]+)/login#n=([^&]+)\z} or die 'bad login link';
    $host = $1;
    my $nonce = $2;
    my $body = '{"nonce":"' . $nonce . '"}';
    my $headers = "X-Cadence-Caller: operator\r\n";
    $headers .= "$extra\r\n" if defined($extra) && length($extra);
    my ($status, $head, $response) = exchange('/api/session', $host, $body, $headers);
    if (defined($capture) && $status =~ /\s200\s/) {
        my ($cookie) = $head =~ /^Set-Cookie:\s*([^;\r\n]+)/im;
        my ($key) = $response =~ /"session_key":"([^"]+)"/;
        die 'operator session missing cookie or page key' unless $cookie && $key;
        open my $out, '>', $capture or die "capture: $!";
        chmod 0600, $capture;
        print {$out} "$cookie\n$key\n";
        close $out;
    }
    print "$status\n";
} elsif ($mode eq 'write') {
    my ($cookie, $key) = @ARGV;
    die 'cookie and page key required' unless $cookie && $key;
    my $headers = "Cookie: $cookie\r\nX-Cadence-Session: $key\r\n"
        . "X-Cadence-Caller: operator\r\nX-Forwarded-User: operator\r\n";
    my ($status) = exchange('/api/issues', $host, '{}', $headers);
    print "$status\n";
} else {
    die "unknown probe mode: $mode";
}
