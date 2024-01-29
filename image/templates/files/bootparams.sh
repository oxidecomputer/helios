#!/bin/bash
#
# Copyright 2024 Oxide Computer Company
#

prtconf -v /devices | awk -v want="$1" "
	/name='/ && /type=string/ && /items=1/ {
		n = \$1;
		sub(\".*name='\", \"\", n);
		sub(\"'.*\", \"\", n);
		next;
	}

	n && /value='/ {
		v = \$1;
		sub(\".*value='\", \"\", v);
		sub(\"'.*\", \"\", v);
		if (want == \"\") {
			printf(\"%s=%s\\n\", n, v);
		} else if (want == n) {
			printf(\"%s\\n\", v);
		}
		n = 0;
		next;
	}
"
