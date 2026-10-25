#!/bin/sh
# Purging also removes the service's settings, usage and log.
set -e
if [ "$1" = purge ]; then
    rm -rf /var/lib/netagg
fi
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || true
fi
