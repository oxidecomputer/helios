#!/bin/ksh93
#
# Copyright 2025 Oxide Computer Company
#

. /lib/svc/share/smf_include.sh

function fatal {
	echo "$@" >&2
	exit $SMF_EXIT_ERR_FATAL
}

if (( $# != 1 )); then
	fatal "usage: compliance-mem-loadgen <jobfile>"
fi

JOB_FILE=$1
STRESS_NG=/opt/ooce/bin/stress-ng

if [[ ! -x $STRESS_NG ]]; then
	 fatal "Cannot find executable file $STRESS_NG"
fi

if [[ ! -f $JOB_FILE ]]; then
	echo "compliance-mem-loadgen: Cannot find job file $JOB_FILE" >&2
	exit $SMF_EXIT_ERR_CONFIG
fi

$STRESS_NG --job  $JOB_FILE &

exit $SMF_EXIT_OK
