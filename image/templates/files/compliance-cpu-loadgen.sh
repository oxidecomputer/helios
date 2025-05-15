#!/bin/ksh93
#
# Copyright 2025 Oxide Computer Company
#

. /lib/svc/share/smf_include.sh

if (( $# != 1 )); then
	echo "usage: compliance-cpu-loadgen.sh <numthreads>" >&2
	exit $SMF_EXIT_ERR_FATAL
fi

integer nthreads=$1

if (( nthreads == 0 )); then
	nthreads=$(psrinfo -t)
fi

for i in {1..$nthreads}; do
	while true; do :; done &
done

exit $SMF_EXIT_OK
