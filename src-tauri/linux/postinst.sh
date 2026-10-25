#!/bin/sh
# Starts the NetAgg service, which runs the engine as root; the tray app runs
# as the user and asks it for everything. On an upgrade, the new binary
# takes over (it comes back on if it was on).
set -e
if [ "$1" = configure ]; then
    # Versions before the service gave the binary capabilities instead.
    setcap -r /usr/bin/netagg 2>/dev/null || true
    if [ -d /run/systemd/system ]; then
        systemctl daemon-reload
        systemctl enable netagg.service
        systemctl restart netagg.service
    fi
fi
