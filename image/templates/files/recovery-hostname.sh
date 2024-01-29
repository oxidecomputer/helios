#!/bin/ksh
#
# Copyright 2024 Oxide Computer Company
#

export PATH=/usr/bin:/usr/sbin:/sbin

set -o errexit
set -o pipefail

sn=$(prtconf -v /devices |
    awk -F"'" 'f { print $2; exit } /baseboard-identifier/ { f=1 }')

if [[ ! -n "$sn" ]]; then
	exit 1
fi

echo "$sn" > /etc/nodename
sed -i -e "s/unknown/$sn/g" /etc/inet/hosts

exit 0
