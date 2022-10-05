#!/bin/sh


diskinfo | grep "^NVME" | awk '{s=(NR==1?s:s ":") "/dev/dsk/" $2 "p0"} END {print s " " NR}' | read NVME_DEVICES NVME_COUNT

fio /root/nvme-max-power.fio --filename ${NVME_DEVICES} --numjobs ${NVME_COUNT}
