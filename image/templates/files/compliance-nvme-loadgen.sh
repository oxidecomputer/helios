#!/bin/ksh93
#
# Copyright 2025 Oxide Computer Company
#

#
# Runs fio (the "Flexible I/O tester") with the given profile.
# By default, the output filenames are derived from NVMe devices
# in the system, but this (among other things) can be overridden
# via environment variables.
#

. /lib/svc/share/smf_include.sh

set -e -o pipefail

function fatal {
	echo "$@" >&2
	exit $SMF_EXIT_ERR_FATAL
}

function config {
	svcprop -c -p config/$1 $SMF_FMRI
}

function booltoint {
	case "$1" in
	false) echo 0;;
	*) echo 1;;
	esac
}

if ! smf_present; then
	fatal "Service Management framework not initialized."
fi


# The only required arguments are the name of the workload, and
# at least one instance.
if (( $# < 2 )); then
	fatal "Usage: compliance-nvme-loadgen workload instance[s]"
fi
workload=$1; shift

ioengine=$(config ioengine)
block_size=$(config block_size)
direct=$(booltoint $(config direct))
buffered=$(booltoint $(config buffered))
iodepth=$(config iodepth)
size=$(config size)
devdir=$(config devdir)

typeset -A nvme_dev_map
pilot local disk ls -H -p -o label,disk |
	sed 's/BSU0/East/;s/BSU1/West/' |
	while read instance device; do
		nvme_dev_map[$instance]=$device
	done

typeset -a out_files
for nvme do
	if [[ ! -v nvme_dev_map[$nvme] ]]; then
		fatal "Unknown device \"$nvme\""
	fi
	dev="${nvme_dev_map[$nvme]}p0"
	out_files+=("$devdir/$dev")
done

env \
	IOENGINE="${ioengine}" \
	BLOCK_SIZE="${block_size}" \
	DIRECT="${direct}" \
	BUFFERED="${buffered}" \
	IODEPTH="${iodepth}" \
	SIZE="${size}" \
	RW="${RW}" \
	FILENAME="${FILENAME:-$(IFS=':'; echo "${out_files[*]}")}" \
	NUMJOBS="${NUMJOBS:-${#out_files[@]}}" \
	fio /root/nvme-${workload}.fio &

exit $SMF_EXIT_OK
