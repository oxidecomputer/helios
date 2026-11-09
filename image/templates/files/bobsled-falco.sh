#!/bin/bash
#
# Copyright 2026 Oxide Computer Company
#

#
# Synchronise the system clock and set up the zpool used by falco.
#

set -o errexit
set -o pipefail
set -o xtrace

. /lib/svc/share/smf_include.sh

if [[ -z $SMF_FMRI ]]; then
	printf 'ERROR: must run under SMF\n' >&2
	exit $SMF_EXIT_ERR_FATAL
fi
pool=$(svcprop -p 'config/pool' "$SMF_FMRI")
pool_size=$(svcprop -p 'config/pool_size' "$SMF_FMRI")

/usr/bin/falco time-sync

#
# On a reboot the pool will already exist on disk, so import it rather than
# creating a new one.
#
if zpool list "$pool" >/dev/null 2>&1; then
	printf 'pool %s already imported\n' "$pool"
elif zpool import -f "$pool"; then
	printf 'imported existing pool %s\n' "$pool"
else
	/usr/bin/falco init-pool "$pool_size"
fi

exit $SMF_EXIT_OK
