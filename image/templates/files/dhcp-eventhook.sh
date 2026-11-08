#!/bin/bash
#
# Copyright 2026 Oxide Computer Company
#

#
# dhcpagent(8) event hook.  Invoked as "eventhook <interface> <event>" with
# stdin/stdout/stderr redirected to /dev/null.  When an IPv4 lease is bound or
# extended, write the DNS servers and domain from the lease to
# /etc/resolv.conf.
#

ifname=$1
event=$2

log() {
	logger -p daemon.notice -t dhcp-eventhook "$*"
}

case $event in
BOUND|EXTEND|INFORM)
	;;
*)
	exit 0
	;;
esac

servers=$(/sbin/dhcpinfo -i "$ifname" DNSserv) || servers=
domain=$(/sbin/dhcpinfo -i "$ifname" DNSdmain) || domain=

if [[ -z $servers ]]; then
	log "$event on $ifname: no DNS servers in lease; leaving resolv.conf"
	exit 0
fi

tmp=/etc/.resolv.conf.$$
{
	printf '# Generated from DHCP lease on %s\n' "$ifname"
	if [[ -n $domain ]]; then
		printf 'domain %s\n' "$domain"
	fi
	for s in $servers; do
		printf 'nameserver %s\n' "$s"
	done
} >"$tmp" || { rm -f "$tmp"; exit 1; }

chmod 0644 "$tmp"
if cmp -s "$tmp" /etc/resolv.conf; then
	rm -f "$tmp"
	exit 0
fi
mv -f "$tmp" /etc/resolv.conf

log "$event on $ifname: resolv.conf updated (servers: $servers;" \
    "domain: ${domain:-none})"
exit 0
