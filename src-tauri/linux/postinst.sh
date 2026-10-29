#!/bin/sh
# Starts the NetAgg service, which runs the engine as root; the tray app runs
# as the user and asks it for everything. On an upgrade, the new binary
# takes over (it comes back on if it was on). The WebUI comes switched off:
# Settings turns it on (netagg webui-switch, through polkit).
set -e
if [ "$1" = configure ]; then
    # Versions before the service gave the binary capabilities instead.
    setcap -r /usr/bin/netagg 2>/dev/null || true
    if ! getent passwd netagg-web >/dev/null; then
        adduser --system --stdoutmsglevel=warn --group --no-create-home --home /nonexistent netagg-web
    fi
    if [ -d /run/systemd/system ]; then
        systemctl daemon-reload
        systemctl enable netagg.service
        systemctl restart netagg.service
        # Only if it was switched on: the new binary takes over.
        systemctl try-restart netagg-webui.service || true
    fi
fi
