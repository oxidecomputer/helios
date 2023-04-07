#!/bin/ksh93
set -A NVMES $(diskinfo | awk '/^NVME/ {print "/dev/dsk/"$2"p0"}')
DEVS=$(IFS=':'; echo "${NVMES[*]}")
NDEVS=${#NVMES[@]}
fio /root/nvme-max-power.fio --filename "${DEVS}" --numjobs "${NDEVS}"
