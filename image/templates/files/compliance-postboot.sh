#!/bin/bash

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

fail=no
for (( i = 0; i < ${#nics[@]}; i++ )); do
	nic=${nics[$i]}

	if ! ipadm show-if "$nic" >/dev/null 2>&1; then
		if ! ipadm create-if -t "$nic" >&2; then
			fail=yes
			continue
		fi
	fi
	if ! ipadm show-addr "$nic/v6" >/dev/null 2>&1; then
		if ! ipadm create-addr -T addrconf -t "$nic/v6" >&2; then
			fail=yes
			continue
		fi
	fi
done

if [[ $fail == yes ]]; then
	exit 1
fi

exit 0
