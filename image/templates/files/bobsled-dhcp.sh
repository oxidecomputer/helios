#!/bin/bash
#
# Copyright 2026 Oxide Computer Company
#

#
# Configure an IPv4 DHCP address on the bench sled management NIC, if it is
# present.  DNS configuration from the lease is written to /etc/resolv.conf by
# the dhcpagent event hook (/etc/dhcp/eventhook).
#

set -o errexit
set -o pipefail
set -o xtrace

. /lib/svc/share/smf_include.sh

if [[ -z $SMF_FMRI ]]; then
	printf 'ERROR: must run under SMF\n' >&2
	exit $SMF_EXIT_ERR_FATAL
fi
link=$(svcprop -p 'config/link' "$SMF_FMRI")
wait=$(svcprop -p 'config/wait_seconds' "$SMF_FMRI")
addrobj="$link/dhcp"

if ! dladm show-link "$link" >/dev/null 2>&1; then
	printf 'link %s not present; skipping DHCP\n' "$link" >&2
	exit $SMF_EXIT_OK
fi

if ! ipadm show-if "$link" >/dev/null 2>&1; then
	ipadm create-if -t "$link"
fi

if ipadm show-addr "$addrobj" >/dev/null 2>&1; then
	exit $SMF_EXIT_OK
fi

#
# Mark this as the primary interface (-1) so that dhcpinfo(1) queries without
# an explicit interface, and the hostname logic in dhcpagent, use it.  If the
# lease does not arrive within the wait period, ipadm returns an error but
# dhcpagent keeps trying in the background; the event hook will update DNS
# whenever the lease is eventually bound, so we do not fail the service.
#
if ! ipadm create-addr -t -T dhcp -1 -w "$wait" "$addrobj"; then
	if ipadm show-addr "$addrobj" >/dev/null 2>&1; then
		printf 'WARNING: no DHCP lease on %s yet; continuing\n' \
		    "$link" >&2
	else
		printf 'ERROR: could not create %s\n' "$addrobj" >&2
		exit $SMF_EXIT_ERR_FATAL
	fi
fi

exit $SMF_EXIT_OK
