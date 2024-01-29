#!/bin/bash
#
# Copyright 2024 Oxide Computer Company
#

set -o errexit
set -o pipefail
set -o xtrace

#
# Find the NICs we want to bring up for IPv6:
#
nics=()
for try in $(dladm show-ether -po link); do
	if [[ $try == igb* ]] || [[ $try == cxgbe* ]]; then
		nics+=( $try )
	fi
done

if (( ${#nics[@]} == 0 )); then
	exit 1
fi

#
# First, ensure any Chelsio NICs are configured to allow for jumbo frames.
#
for (( i = 0; i < ${#nics[@]}; i++ )); do
	nic=${nics[$i]}

	if [[ $nic != cxgbe* ]]; then
		continue
	fi

	if ! mtu=$(dladm show-linkprop -o value -c -p mtu "$nic"); then
		printf 'WARNING: could not get MTU for %s?\n' "$nic" >&2
		continue
	fi

	want=9000
	if [[ $mtu == $want ]]; then
		continue
	fi

	if ! dladm set-linkprop -p "mtu=$want" "$nic"; then
		printf 'WARNING: could not set MTU for %s?\n' "$nic" >&2
	fi
done

fail=no
for (( i = 0; i < ${#nics[@]}; i++ )); do
	nic=${nics[$i]}

	if ! ipadm show-if "$nic" >/dev/null 2>&1; then
		if ! ipadm create-if -t "$nic" >&2; then
			fail=yes
			continue
		fi
	fi
	if ! ipadm show-addr "$nic/ll" >/dev/null 2>&1; then
		if ! ipadm create-addr -T addrconf -t "$nic/ll" >&2; then
			fail=yes
			continue
		fi
	fi
done

if [[ $fail == yes ]]; then
	exit 1
fi

exit 0
