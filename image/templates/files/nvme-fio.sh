#!/bin/ksh93
#
# Runs fio (the "Flexible I/O tester") with the given profile.
# By default, the output filenames are derived from NVMe devices
# in the system, but this (among other things) can be overridden
# via environment variables.
#
# Because this can be very destructive if not used carefully
# (fio will happily open a device file and clobber anything on
# the associated device), this prints the command it would run,
# in a manner that can be inspected and then copied or piped
# into a shell.
#

#
# Copyright 2024 Oxide Computer Company
#

#
# The only required argument is the name of the config.
#
if [[ "$#" != 1 ]]; then
	echo "Usage: nvme-fio.sh config" >&2
	exit 1
fi
CONFIG=$1
OUTDIR=${OUTDIR:-/fio}
typeset -a NVMES=($(diskinfo -Hp | awk '/^NVME/ {print "'"${OUTDIR}"'/"$2}'))

echo env \
    IOENGINE="${IOENGINE:-pvsync}" \
    BLOCK_SIZE="${BLOCK_SIZE:-4k}" \
    DIRECT="${DIRECT:-1}" \
    BUFFERED="${BUFFERED:-0}" \
    IODEPTH="${IODEPTH:-64}" \
    SIZE="${SIZE:-64g}" \
    RW="${RW}" \
    FILENAME="${FILENAME:-$(IFS=':'; echo "${NVMES[*]}")}" \
    NUMJOBS="${NUMJOBS:-${#NVMES[@]}}" \
    fio nvme-${CONFIG}.fio
