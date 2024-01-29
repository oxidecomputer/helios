#!/bin/bash
#
# Copyright 2024 Oxide Computer Company
#

set -o errexit
set -o pipefail
set -o xtrace

#
# Use the system serial number for the hostname:
#
if sn=$(pilot gimlet info -i); then
	echo "$sn" > /etc/nodename
	sed -i -e "s/unknown/$sn/g" /etc/inet/hosts
fi

exit 0
