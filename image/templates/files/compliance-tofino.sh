#!/bin/bash

set -o errexit
set -o pipefail
set -o xtrace

while :; do
	if [[ -c /dev/tofino ]]; then
		#
		# We have found a sidecar, so start the switch zone!
		#
		exec pilot dendrite create
	fi

	sleep 1
done
