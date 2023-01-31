#!/bin/bash

set -o errexit
set -o pipefail
set -o xtrace

exec pilot host announce
