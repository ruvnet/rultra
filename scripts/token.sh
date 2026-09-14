#!/usr/bin/env bash
# Fetch a console token from GCP Secret Manager.
#
#   scripts/token.sh            the control token
#   scripts/token.sh read       the read-only token
#
# The value is never written to a file and never committed. /etc/rultra/ui.env
# on the box holds the same value at mode 0600; Secret Manager is the record of
# truth so a reflashed board can be reconfigured without inventing a new one.
set -euo pipefail
PROJECT="${RULTRA_GCP_PROJECT:-cognitum-20260110}"
case "${1:-control}" in
  read) NAME=RULTRA_CONSOLE_READ_TOKEN ;;
  control) NAME=RULTRA_CONSOLE_TOKEN ;;
  *) echo "usage: $0 [control|read]" >&2; exit 2 ;;
esac
gcloud secrets versions access latest --secret="$NAME" --project="$PROJECT"
