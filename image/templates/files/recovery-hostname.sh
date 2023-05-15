#!/bin/ksh

export PATH=/usr/bin:/usr/sbin:/sbin

set -o errexit
set -o pipefail

sn=$(prtconf -v /devices | \
    awk -F"'" 'f { print $2; exit } /baseboard-identifier/ { f=1 }')

[[ -n "$sn" ]] || exit 1

echo "$sn" > /etc/nodename
sed -i -e "s/unknown/$sn/g" /etc/inet/hosts

exit 0
