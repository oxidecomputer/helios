#!/bin/bash
#
# Copyright 2024 Oxide Computer Company
#

set -o errexit
set -o pipefail
set -o xtrace

cat >/dev/msglog <<EOF

##################################################
####     #############                          ##
###  ###  ############                          ##
##  ### #  ##  ###  ##  Oxide Computer Company  ##
##  ## ##  ###  #  ###                          ##
##  # ###  ####   ####    This Station Under    ##
###  ###  ####  #  ###     Computer Control     ##
####     ####  ###  ##                          ##
##################################################

EOF

if zpool import -f data; then
	if [[ -f /data/postboot/bin/postboot.sh ]]; then
		exec /bin/bash /data/postboot/bin/postboot.sh
	else
		printf 'WARNING: no /data/postboot/bin/postboot.sh\n' \
		    >/dev/msglog
	fi
else
	printf 'WARNING: no data pool could be imported\n' \
	    >/dev/msglog
fi

exit 0
