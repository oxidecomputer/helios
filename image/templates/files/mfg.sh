#!/bin/bash
#
# Copyright 2025 Oxide Computer Company
#

set -o pipefail

#
# Find the PCI NIC we want; it will either use the driver igb, e1000g, or ixgbe.
#
nic=
for try in $(dladm show-ether -po link); do
	if [[ $try != igb* ]] && [[ $try != e1000g* ]] &&
	    [[ $try != ixgbe* ]]; then
		continue
	fi

	nic=$try
	break
done

if [[ -z $nic ]]; then
	printf 'ERROR: no PCI NIC?\n' >&2
	exit 1
fi

#
# Ping the all-hosts multicast address through that interface so that we can be
# detected by the manufacturing control station.
#
while :; do
	ping -s -A inet6 -i "$nic" -n ff02::1 >/dev/null 2>&1
	sleep 1
done
