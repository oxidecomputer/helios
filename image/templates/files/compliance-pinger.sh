#!/bin/ksh93
#
# Copyright 2025 Oxide Computer Company
#

. /lib/svc/share/smf_include.sh

function fatal {
	echo "$@" >&2
	exit $SMF_EXIT_ERR_FATAL
}

# The only required argument is the name of the interface (the service instance
# name).
typeset -r intf="$1"
[[ -z "$intf" ]] && fatal "Usage: compliance-pinger <interface>"

smf_present || fatal "Service Management framework not initialized."

smf_clear_env
/usr/sbin/ping -I 0.01 -sn ff02::1%$intf >/dev/null &

exit $SMF_EXIT_OK
