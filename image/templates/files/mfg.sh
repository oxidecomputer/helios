#!/bin/bash

set -o pipefail

#
# Find the PCI NIC we want; it will either use driver igb or e1000g.
# If we don't see one yet, we'll sleep and wait for one to be inserted.
#
while :; do
	nic=
	for try in $(dladm show-ether -po link); do
		if [[ $try != igb* ]] && [[ $try != e1000g* ]]; then
			continue
		fi

		nic=$try
		break
	done

	if [[ -n $nic ]]; then
		break
	fi


	printf 'ERROR: no PCI NIC?\n' >&2
	sleep 5
done

#
# Bring an IPv6 link local address up on the NIC we have selected:
#
if ! ipadm show-if "$nic" >/dev/null 2>&1; then
	printf 'creating interface %s\n' "$nic"
	if ! ipadm create-if -t "$nic"; then
		exit 1
	fi
fi
if ! ipadm show-addr "$nic/v6" >/dev/null 2>&1; then
	printf 'creating address %s/v6\n' "$nic"
	if ! ipadm create-addr -T addrconf -t "$nic/v6"; then
		exit 1
	fi
fi

#
# Ping the all-hosts multicast address through that interface so that we can be
# detected by the manufacturing control station.
#
while :; do
	ping -s -A inet6 -i "$nic" -n ff02::1 >/dev/null 2>&1
	sleep 1
done
